//! Node.js / JDK / Maven version detection, install and env switching (Windows).
//!
//! Rewrites User (and when needed Machine) PATH / JAVA_HOME / MAVEN_HOME so new
//! shells pick up the selected runtime.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::Emitter;
use tokio::io::AsyncWriteExt;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Give the child a brand-new console. The app is a console-subsystem program (no
/// `windows_subsystem` attribute anywhere), so a plain `cmd /k` attaches to the app's
/// own console: it runs, but no terminal window ever appears.
#[cfg(target_os = "windows")]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Node,
    Jdk,
    Maven,
    Python,
}

impl RuntimeKind {
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "node" => Ok(RuntimeKind::Node),
            "jdk" | "java" => Ok(RuntimeKind::Jdk),
            "maven" | "mvn" => Ok(RuntimeKind::Maven),
            "python" | "py" => Ok(RuntimeKind::Python),
            other => Err(format!("Unknown runtime kind: {}", other)),
        }
    }

    /// Value stored in `RuntimeVersion.kind` and used as the managed sub-directory name.
    fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::Node => "node",
            RuntimeKind::Jdk => "jdk",
            RuntimeKind::Maven => "maven",
            RuntimeKind::Python => "python",
        }
    }

    /// PATH entries one install contributes, `bin` first. Python puts its tools
    /// (pip, and every console script pip installs) in `Scripts` next to python.exe.
    fn path_entries(&self, bin: &str) -> Vec<String> {
        match self {
            RuntimeKind::Python => vec![
                bin.to_string(),
                Path::new(bin).join("Scripts").to_string_lossy().to_string(),
            ],
            _ => vec![bin.to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeVersion {
    pub kind: String,
    pub version: String,
    pub path: String,
    pub bin_path: String,
    pub source: String,
    pub active: bool,
    pub custom: bool,
    pub on_machine_path: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchPlan {
    pub needs_elevation: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub message: String,
    pub version: String,
    pub verified: bool,
    pub verified_version: Option<String>,
    pub elevated: bool,
    /// 切换后的版本列表（含新的 active 标记）。
    pub list: Vec<RuntimeVersion>,
}

/// `serde(default)`: files written before a kind existed lack its key, and a parse
/// failure would silently drop every other custom path.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct CustomRuntimesFile {
    node: Vec<String>,
    jdk: Vec<String>,
    maven: Vec<String>,
    python: Vec<String>,
}

impl CustomRuntimesFile {
    fn paths_mut(&mut self, kind: &RuntimeKind) -> &mut Vec<String> {
        match kind {
            RuntimeKind::Node => &mut self.node,
            RuntimeKind::Jdk => &mut self.jdk,
            RuntimeKind::Maven => &mut self.maven,
            RuntimeKind::Python => &mut self.python,
        }
    }
}

fn hidden_cmd(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn app_data_dir() -> Result<PathBuf, String> {
    let appdata =
        std::env::var("APPDATA").map_err(|_| "Failed to resolve APPDATA".to_string())?;
    let dir = Path::new(&appdata).join("com.ai-workbench.app");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data dir: {}", e))?;
    Ok(dir)
}

fn custom_file_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("custom-runtimes.json"))
}

fn load_customs() -> CustomRuntimesFile {
    let Ok(path) = custom_file_path() else {
        return CustomRuntimesFile::default();
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return CustomRuntimesFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_customs(data: &CustomRuntimesFile) -> Result<(), String> {
    let path = custom_file_path()?;
    let raw = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    fs::write(path, raw).map_err(|e| format!("Failed to save custom runtimes: {}", e))
}

fn normalize_path(p: &Path) -> String {
    fs::canonicalize(p)
        .map(|c| c.to_string_lossy().trim_start_matches(r"\\?\").to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

fn path_key(p: &str) -> String {
    p.trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn path_eq(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

/// Component-wise version comparison, so `20.11.0` sorts above `9.11.1` (a plain
/// string compare puts the latter first) and the JDK update number counts as a
/// component (`1.8.0_301` > `1.8.0_91`). Non-numeric tails only break ties, so
/// `21.0.0-ea` still ranks below `21.0.0`.
pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parts(value: &str) -> (Vec<u64>, String) {
        let mut numbers = Vec::new();
        let mut letters = String::new();
        let mut digits = String::new();
        // `.split()` would drop the leading 'v' but not a second one; trim is enough.
        for ch in value.trim().trim_start_matches(['v', 'V']).chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                continue;
            }
            if !digits.is_empty() {
                numbers.push(digits.parse::<u64>().unwrap_or(0));
                digits.clear();
            }
            if ch.is_ascii_alphabetic() {
                letters.push(ch.to_ascii_lowercase());
            }
        }
        if !digits.is_empty() {
            numbers.push(digits.parse::<u64>().unwrap_or(0));
        }
        (numbers, letters)
    }

    let (a_numbers, a_letters) = parts(a);
    let (b_numbers, b_letters) = parts(b);
    a_numbers
        .cmp(&b_numbers)
        .then_with(|| a_letters.cmp(&b_letters))
}

fn run_capture(exe: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = hidden_cmd(exe.to_string_lossy().as_ref());
    cmd.args(args);
    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stdout.is_empty() {
        Some(stdout)
    } else if !stderr.is_empty() {
        Some(stderr)
    } else {
        None
    }
}

/// 按可执行文件 mtime 缓存版本号：列表刷新一次要对每个安装起一个 `node -v` / `java -version`，
/// 同一个 exe 没变过就不必再跑。
fn cached_version(exe: &Path, probe: impl FnOnce() -> Option<String>) -> Option<String> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, (std::time::SystemTime, String)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mtime = fs::metadata(exe).and_then(|m| m.modified()).ok();
    if let Some(mtime) = mtime {
        if let Some((t, v)) = cache.lock().ok()?.get(exe) {
            if *t == mtime {
                return Some(v.clone());
            }
        }
    }
    let version = probe()?;
    if let (Some(mtime), Ok(mut map)) = (mtime, cache.lock()) {
        map.insert(exe.to_path_buf(), (mtime, version.clone()));
    }
    Some(version)
}

fn read_node_version(bin_dir: &Path) -> Option<String> {
    let exe = bin_dir.join("node.exe");
    if !exe.is_file() {
        let exe_unix = bin_dir.join("node");
        if exe_unix.is_file() {
            return run_capture(&exe_unix, &["-v"]).map(|v| v.trim_start_matches('v').to_string());
        }
        return None;
    }
    cached_version(&exe, || {
        run_capture(&exe, &["-v"]).map(|v| v.trim_start_matches('v').to_string())
    })
}

fn parse_java_version(out: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let idx = line.find('"')?;
        let rest = &line[idx + 1..];
        rest.find('"').map(|end| rest[..end].to_string())
    })
}

fn read_jdk_version(home: &Path) -> Option<String> {
    let release = home.join("release");
    if let Ok(content) = fs::read_to_string(&release) {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("JAVA_VERSION=") {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    let java = home.join("bin").join("java.exe");
    if java.is_file() {
        return cached_version(&java, || {
            run_capture(&java, &["-version"]).and_then(|out| parse_java_version(&out))
        });
    }
    None
}

fn is_jdk_home(dir: &Path) -> bool {
    dir.join("bin").join("java.exe").is_file()
}

fn is_node_bin(dir: &Path) -> bool {
    dir.join("node.exe").is_file()
}

fn is_maven_home(dir: &Path) -> bool {
    dir.join("bin").join("mvn.cmd").is_file() && dir.join("lib").is_dir()
}

/// `lib/maven-core-3.9.6.jar` → `3.9.6`. Reading the jar name avoids `mvn -v`, which
/// needs a working JAVA_HOME and takes about a second per install.
fn read_maven_version(home: &Path) -> Option<String> {
    fs::read_dir(home.join("lib")).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        name.strip_prefix("maven-core-")
            .and_then(|rest| rest.strip_suffix(".jar"))
            .map(str::to_string)
    })
}

/// Maven homes up to `depth` levels below `root` (wrapper dists nest them under a hash dir).
fn scan_maven_tree(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 || !root.is_dir() {
        return;
    }
    if is_maven_home(root) {
        out.push(root.to_path_buf());
        return;
    }
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                scan_maven_tree(&p, out, depth - 1);
            }
        }
    }
}

fn scan_maven_dirs(snap: &EnvSnapshot) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mut try_add = |home: PathBuf, source: &str| {
        if is_maven_home(&home) {
            let key = normalize_path(&home);
            if found.iter().any(|(h, _)| path_eq(&normalize_path(h), &key)) {
                return;
            }
            found.push((home, source.to_string()));
        }
    };

    for bin in discover_from_where("mvn.cmd")
        .into_iter()
        .chain(discover_bins_on_path("mvn.cmd", snap))
    {
        if let Some(home) = bin.parent() {
            try_add(home.to_path_buf(), "path");
        }
    }

    for var in ["MAVEN_HOME", "M2_HOME"] {
        if let Ok(home) = std::env::var(var) {
            try_add(PathBuf::from(home), "env");
        }
    }

    for root in [r"C:\Program Files\Apache", r"C:\Program Files\Maven", r"C:\Program Files\apache-maven"] {
        let mut homes = Vec::new();
        scan_maven_tree(Path::new(root), &mut homes, 2);
        for home in homes {
            try_add(home, "system");
        }
    }

    if let Ok(home) = std::env::var("USERPROFILE") {
        let home = Path::new(&home);
        let sources: [(PathBuf, &str, usize); 3] = [
            (home.join(".m2").join("wrapper").join("dists"), "wrapper", 4),
            (home.join("scoop").join("apps").join("maven"), "scoop", 2),
            (home.join(".sdkman").join("candidates").join("maven"), "sdkman", 2),
        ];
        for (root, source, depth) in sources {
            let mut homes = Vec::new();
            scan_maven_tree(&root, &mut homes, depth);
            for h in homes {
                if h.file_name().and_then(|n| n.to_str()) != Some("current") {
                    try_add(h, source);
                }
            }
        }
    }

    if let Ok(managed) = managed_kind_dir(&RuntimeKind::Maven) {
        if let Ok(entries) = fs::read_dir(&managed) {
            for entry in entries.flatten() {
                try_add(entry.path(), "managed");
            }
        }
    }

    found
}

/// A real interpreter install. `Lib\os.py` rules out the WindowsApps `python.exe`
/// Store stub and virtualenvs (whose python.exe sits in `Scripts` without a stdlib).
fn is_python_home(dir: &Path) -> bool {
    dir.join("python.exe").is_file() && dir.join("Lib").join("os.py").is_file()
}

/// `Python 3.12.1` → `3.12.1`. Python 2 prints the banner to stderr, which
/// `run_capture` falls back to.
fn parse_python_version(out: &str) -> Option<String> {
    let v = out.lines().next()?.trim().strip_prefix("Python ")?.trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn read_python_version(home: &Path) -> Option<String> {
    let exe = home.join("python.exe");
    cached_version(&exe, || run_capture(&exe, &["-V"]).and_then(|out| parse_python_version(&out)))
}

/// Install dirs registered under PEP 514 (`Software\Python\<Company>\<Tag>\InstallPath`):
/// python.org installers, the Python install manager and uv all write them. Any REG_SZ
/// that names a python.exe or a dir holding one is a candidate; `is_python_home` filters.
#[cfg(target_os = "windows")]
fn python_registry_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    for key in [
        r"HKCU\Software\Python",
        r"HKLM\Software\Python",
        r"HKLM\Software\WOW6432Node\Python",
    ] {
        let Ok(output) = hidden_cmd("reg").args(["query", key, "/s"]).output() else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((_, value)) = line.split_once("REG_SZ") else {
                continue;
            };
            let value = value.trim().trim_end_matches('\\');
            if value.is_empty() || value.contains(';') {
                continue;
            }
            let p = PathBuf::from(value);
            let home = if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("python.exe")) {
                p.parent().map(Path::to_path_buf)
            } else {
                Some(p)
            };
            if let Some(home) = home {
                homes.push(home);
            }
        }
    }
    homes
}

fn scan_python_dirs(snap: &EnvSnapshot) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mut try_add = |home: PathBuf, source: &str| {
        if is_python_home(&home) {
            let key = normalize_path(&home);
            if found.iter().any(|(h, _)| path_eq(&normalize_path(h), &key)) {
                return;
            }
            found.push((home, source.to_string()));
        }
    };

    for dir in discover_from_where("python.exe")
        .into_iter()
        .chain(discover_bins_on_path("python.exe", snap))
    {
        try_add(dir, "path");
    }

    #[cfg(target_os = "windows")]
    for home in python_registry_homes() {
        try_add(home, "registry");
    }

    let children = |root: &Path| -> Vec<PathBuf> {
        fs::read_dir(root)
            .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default()
    };
    let starts_with_python = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().starts_with("python"))
    };

    for root in [r"C:\", r"C:\Program Files", r"C:\Program Files (x86)"] {
        for p in children(Path::new(root)).into_iter().filter(|p| starts_with_python(p)) {
            try_add(p, "system");
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = Path::new(&local);
        for root in [local.join("Programs").join("Python"), local.join("Python")] {
            for p in children(&root) {
                try_add(p, "system");
            }
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        for p in children(&Path::new(&appdata).join("uv").join("python")) {
            try_add(p, "uv");
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        let home = Path::new(&home);
        for p in children(&home.join(".pyenv").join("pyenv-win").join("versions")) {
            try_add(p, "pyenv");
        }
        for p in children(&home.join("scoop").join("apps").join("python")) {
            if p.file_name().and_then(|n| n.to_str()) != Some("current") {
                try_add(p, "scoop");
            }
        }
        for name in ["anaconda3", "miniconda3", "miniforge3", "mambaforge"] {
            try_add(home.join(name), "conda");
        }
    }

    if let Ok(managed) = managed_kind_dir(&RuntimeKind::Python) {
        for p in children(&managed) {
            try_add(p, "managed");
        }
    }

    found
}

fn push_unique(list: &mut Vec<RuntimeVersion>, item: RuntimeVersion) {
    if list
        .iter()
        .any(|x| path_eq(&x.path, &item.path) || path_eq(&x.bin_path, &item.bin_path))
    {
        return;
    }
    list.push(item);
}

fn scan_jdk_tree(root: &Path, source: &str, out: &mut Vec<(PathBuf, String)>, depth: usize) {
    if depth == 0 || !root.is_dir() {
        return;
    }
    if is_jdk_home(root) {
        out.push((root.to_path_buf(), source.to_string()));
        return;
    }
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                scan_jdk_tree(&p, source, out, depth - 1);
            }
        }
    }
}

fn discover_from_where(exe_name: &str) -> Vec<PathBuf> {
    let mut bins = Vec::new();
    if let Ok(output) = hidden_cmd("cmd").args(["/c", "where", exe_name]).output() {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let p = PathBuf::from(line);
                if let Some(parent) = p.parent() {
                    bins.push(parent.to_path_buf());
                }
            }
        }
    }
    bins
}

fn split_path(path: &str) -> Vec<String> {
    path.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn join_path(parts: &[String]) -> String {
    parts.join(";")
}

fn env_get(name: &str, target: &str) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "[Environment]::GetEnvironmentVariable('{}','{}')",
            name.replace('\'', "''"),
            target.replace('\'', "''")
        );
        let output = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .map_err(|e| format!("Failed to read {} env {}: {}", target, name, e))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Failed to read {} env {}: {}", target, name, err));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = target;
        Ok(std::env::var(name).unwrap_or_default())
    }
}

fn user_env_get(name: &str) -> Result<String, String> {
    env_get(name, "User")
}

fn machine_env_get(name: &str) -> Result<String, String> {
    env_get(name, "Machine")
}

/// One-shot registry env read for a scan/switch pass (avoids repeated PowerShell).
pub(crate) struct EnvSnapshot {
    pub(crate) user_path: String,
    pub(crate) machine_path: String,
    pub(crate) user_java_home: String,
    pub(crate) machine_java_home: String,
}

impl EnvSnapshot {
    /// PATH as a freshly launched process sees it: Machine first, then User.
    pub(crate) fn effective_path(&self) -> String {
        match (self.machine_path.trim().is_empty(), self.user_path.trim().is_empty()) {
            (true, _) => self.user_path.clone(),
            (false, true) => self.machine_path.clone(),
            (false, false) => format!("{};{}", self.machine_path, self.user_path),
        }
    }
}

/// Environment values of a registry key, read with `reg.exe` and expanded the way a
/// REG_EXPAND_SZ value promises. `reg query` costs ~90ms where the PowerShell cmdlet
/// below costs ~1.5s — and this snapshot is read on every runtime scan and DSH start.
#[cfg(target_os = "windows")]
fn reg_query_env(key: &str) -> Option<HashMap<String, String>> {
    let output = hidden_cmd("reg").args(["query", key]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let mut values = HashMap::new();
    for line in text.lines() {
        // The key header line has no whitespace, so it never reaches the value split.
        let trimmed = line.trim_start();
        let Some((name, rest)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some((kind, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if !kind.starts_with("REG_") {
            continue;
        }
        values.insert(name.to_string(), expand_env_vars(value.trim_start()));
    }
    Some(values)
}

/// Expand `%NAME%` references against this process's environment. Unknown names stay
/// verbatim rather than being dropped, so a partial expansion cannot corrupt a PATH entry.
#[cfg(target_os = "windows")]
fn expand_env_vars(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push('%');
            out.push_str(after);
            return out;
        };
        let name = &after[..end];
        match std::env::var(name) {
            Ok(expanded) if !name.is_empty() => out.push_str(&expanded),
            _ => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn load_env_snapshot() -> EnvSnapshot {
    #[cfg(target_os = "windows")]
    {
        // Fast path first: two `reg` calls instead of a ~1.5s PowerShell start.
        const USER_ENV_KEY: &str = r"HKCU\Environment";
        const MACHINE_ENV_KEY: &str =
            r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
        if let (Some(user), Some(machine)) =
            (reg_query_env(USER_ENV_KEY), reg_query_env(MACHINE_ENV_KEY))
        {
            return EnvSnapshot {
                user_path: user.get("Path").cloned().unwrap_or_default(),
                machine_path: machine.get("Path").cloned().unwrap_or_default(),
                user_java_home: user.get("JAVA_HOME").cloned().unwrap_or_default(),
                machine_java_home: machine.get("JAVA_HOME").cloned().unwrap_or_default(),
            };
        }

        let script = r#"
$up = [Environment]::GetEnvironmentVariable('Path','User')
$mp = [Environment]::GetEnvironmentVariable('Path','Machine')
$uj = [Environment]::GetEnvironmentVariable('JAVA_HOME','User')
$mj = [Environment]::GetEnvironmentVariable('JAVA_HOME','Machine')
Write-Output ("UP=" + $(if ($null -eq $up) { '' } else { $up }))
Write-Output ("MP=" + $(if ($null -eq $mp) { '' } else { $mp }))
Write-Output ("UJ=" + $(if ($null -eq $uj) { '' } else { $uj }))
Write-Output ("MJ=" + $(if ($null -eq $mj) { '' } else { $mj }))
"#;
        if let Ok(output) = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
        {
            if output.status.success() {
                let mut snap = EnvSnapshot {
                    user_path: String::new(),
                    machine_path: String::new(),
                    user_java_home: String::new(),
                    machine_java_home: String::new(),
                };
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    if let Some(rest) = line.strip_prefix("UP=") {
                        snap.user_path = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("MP=") {
                        snap.machine_path = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("UJ=") {
                        snap.user_java_home = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("MJ=") {
                        snap.machine_java_home = rest.to_string();
                    }
                }
                return snap;
            }
        }
    }
    EnvSnapshot {
        user_path: user_env_get("Path")
            .or_else(|_| user_env_get("PATH"))
            .unwrap_or_default(),
        machine_path: machine_env_get("Path")
            .or_else(|_| machine_env_get("PATH"))
            .unwrap_or_default(),
        user_java_home: user_env_get("JAVA_HOME").unwrap_or_default(),
        machine_java_home: machine_env_get("JAVA_HOME").unwrap_or_default(),
    }
}

fn discover_bins_on_path(exe_name: &str, snap: &EnvSnapshot) -> Vec<PathBuf> {
    let mut bins = Vec::new();
    let mut consider = |path: &str| {
        for part in split_path(path) {
            let dir = PathBuf::from(&part);
            if dir.join(exe_name).is_file() {
                bins.push(dir);
            }
        }
    };
    if let Ok(path) = std::env::var("PATH") {
        consider(&path);
    }
    consider(&snap.user_path);
    consider(&snap.machine_path);
    bins
}

fn scan_node_dirs(snap: &EnvSnapshot) -> Vec<(PathBuf, PathBuf, String)> {
    let mut found: Vec<(PathBuf, PathBuf, String)> = Vec::new();

    let mut try_add = |root: PathBuf, bin: PathBuf, source: &str| {
        if is_node_bin(&bin) {
            let bin_key = normalize_path(&bin);
            if found
                .iter()
                .any(|(_, b, _)| path_eq(&normalize_path(b), &bin_key))
            {
                return;
            }
            found.push((root, bin, source.to_string()));
        }
    };

    try_add(
        PathBuf::from(r"C:\Program Files\nodejs"),
        PathBuf::from(r"C:\Program Files\nodejs"),
        "system",
    );
    try_add(
        PathBuf::from(r"C:\Program Files (x86)\nodejs"),
        PathBuf::from(r"C:\Program Files (x86)\nodejs"),
        "system",
    );

    for bin in discover_from_where("node")
        .into_iter()
        .chain(discover_bins_on_path("node.exe", snap))
    {
        try_add(bin.clone(), bin, "path");
    }

    if let Ok(nvm_home) = std::env::var("NVM_HOME") {
        let home = PathBuf::from(&nvm_home);
        if let Ok(entries) = fs::read_dir(&home) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with('v')
                        || name
                            .chars()
                            .next()
                            .map(|c| c.is_ascii_digit())
                            .unwrap_or(false)
                    {
                        try_add(p.clone(), p.clone(), "nvm");
                    }
                }
            }
        }
    }
    if let Ok(nvm_symlink) = std::env::var("NVM_SYMLINK") {
        let p = PathBuf::from(nvm_symlink);
        try_add(p.clone(), p, "nvm");
    }

    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let fnm = Path::new(&local).join("fnm").join("node-versions");
        if let Ok(entries) = fs::read_dir(&fnm) {
            for entry in entries.flatten() {
                let installation = entry.path().join("installation");
                if installation.is_dir() {
                    try_add(installation.clone(), installation, "fnm");
                }
            }
        }
        let volta = Path::new(&local)
            .join("Volta")
            .join("tools")
            .join("image")
            .join("node");
        if let Ok(entries) = fs::read_dir(&volta) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    try_add(p.clone(), p, "volta");
                }
            }
        }
    }

    if let Ok(home) = std::env::var("USERPROFILE") {
        let nvm = Path::new(&home).join(".nvm").join("versions").join("node");
        if let Ok(entries) = fs::read_dir(&nvm) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    try_add(p.clone(), p, "nvm");
                }
            }
        }
    }

    if let Ok(managed) = managed_kind_dir(&RuntimeKind::Node) {
        if let Ok(entries) = fs::read_dir(&managed) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    try_add(p.clone(), p, "managed");
                }
            }
        }
    }

    found
}

fn scan_jdk_dirs(snap: &EnvSnapshot) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mut try_add = |home: PathBuf, source: &str| {
        if is_jdk_home(&home) {
            let key = normalize_path(&home);
            if found.iter().any(|(h, _)| path_eq(&normalize_path(h), &key)) {
                return;
            }
            found.push((home, source.to_string()));
        }
    };

    let roots: Vec<PathBuf> = [
        r"C:\Program Files\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files\Zulu",
        r"C:\Program Files\BellSoft",
        r"C:\Program Files\Semeru",
        r"C:\Program Files\SapMachine",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        if is_jdk_home(&root) {
            try_add(root.clone(), "system");
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if name.contains("jdk")
                    || name.contains("jre")
                    || name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
                {
                    try_add(p, "system");
                }
            }
        }
    }

    if let Ok(home) = std::env::var("USERPROFILE") {
        let jdks = Path::new(&home).join(".jdks");
        if let Ok(entries) = fs::read_dir(jdks) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    try_add(p, "ide");
                }
            }
        }
        let sdkman = Path::new(&home).join(".sdkman").join("candidates").join("java");
        if let Ok(entries) = fs::read_dir(sdkman) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.file_name().and_then(|n| n.to_str()) != Some("current") {
                    try_add(p, "sdkman");
                }
            }
        }
        let mut jvman_found = Vec::new();
        scan_jdk_tree(&Path::new(&home).join(".jvman"), "jvman", &mut jvman_found, 6);
        for (p, src) in jvman_found {
            try_add(p, &src);
        }
    }

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let p = PathBuf::from(java_home);
        if is_jdk_home(&p) {
            try_add(p, "env");
        }
    }

    for bin in discover_from_where("java")
        .into_iter()
        .chain(discover_bins_on_path("java.exe", snap))
    {
        // Skip Oracle javapath shim directory
        let name = bin
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name == "javapath" {
            continue;
        }
        if let Some(home) = bin.parent() {
            if is_jdk_home(home) {
                try_add(home.to_path_buf(), "path");
            }
        }
    }

    if let Ok(managed) = managed_kind_dir(&RuntimeKind::Jdk) {
        if let Ok(entries) = fs::read_dir(&managed) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    try_add(p, "managed");
                }
            }
        }
    }

    found
}

/// 通知已运行的程序（资源管理器等）环境变量变了。原生调用代替旧的 PowerShell + Add-Type
/// （每次现编 C# 要 1~3 秒）；放后台线程，挂起的窗口最多拖 1 秒也不阻塞切换结果。
fn broadcast_env_change() {
    #[cfg(target_os = "windows")]
    std::thread::spawn(|| {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let area: Vec<u16> = "Environment".encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(PCWSTR(area.as_ptr()).0 as isize),
                SMTO_ABORTIFHUNG,
                1000,
                None,
            );
        }
    });
}

/// 直写 `HKCU\Environment`（不起 PowerShell，也不触发 .NET 自带的逐窗口广播）。
/// `value` 为 `None` 时删除该项；`Path` 等含 `%VAR%` 的值以 REG_EXPAND_SZ 存。
fn user_env_set_opt(name: &str, value: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_SET_VALUE, REG_EXPAND_SZ,
        };
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let key_path = wide("Environment");
        let name_w = wide(name);
        let mut hkey = HKEY::default();
        let opened = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(key_path.as_ptr()), Some(0), KEY_SET_VALUE, &mut hkey)
        };
        if opened.is_err() {
            return Err(format!("无法打开用户环境变量注册表项（{}）", opened.0));
        }
        let result = match value {
            Some(v) => {
                let data: Vec<u8> = wide(v).iter().flat_map(|c| c.to_le_bytes()).collect();
                unsafe { RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), Some(0), REG_EXPAND_SZ, Some(&data)) }
            }
            None => {
                let r = unsafe { RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr())) };
                // 本来就没有这一项也算成功。
                if r.0 == 2 { windows::Win32::Foundation::ERROR_SUCCESS } else { r }
            }
        };
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if result.is_err() {
            return Err(format!("写入用户环境变量 {} 失败（{}）", name, result.0));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (name, value);
        Err("Runtime switching is only supported on Windows".to_string())
    }
}

fn user_env_set(name: &str, value: &str) -> Result<(), String> {
    user_env_set_opt(name, Some(value))
}

fn user_env_exists(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        reg_query_env(r"HKCU\Environment")
            .map(|values| values.keys().any(|k| k.eq_ignore_ascii_case(name)))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = name;
        false
    }
}

const MACHINE_ENV_PS_KEY: &str = r"HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// 提权脚本：直接改注册表（`[Environment]::SetEnvironmentVariable` 会逐窗口广播，慢），
/// 广播统一由调用方最后做一次。`None` 表示删除该项。
fn machine_env_script(ops: &[(&str, Option<&str>)]) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut lines = vec!["$ErrorActionPreference = 'Stop'".to_string(), format!("$k = {}", q(MACHINE_ENV_PS_KEY))];
    for (name, value) in ops {
        lines.push(match value {
            Some(v) => format!("Set-ItemProperty -Path $k -Name {} -Value {} -Type ExpandString", q(name), q(v)),
            None => format!("Remove-ItemProperty -Path $k -Name {} -ErrorAction SilentlyContinue", q(name)),
        });
    }
    lines.push("exit 0".to_string());
    lines.join("\n")
}

/// 一次 UAC 写完全部系统级变量。`-Verb RunAs` 起的进程拿不到父进程的
/// CREATE_NO_WINDOW，必须显式 `-WindowStyle Hidden`，否则会露出蓝色管理员 PowerShell 窗口。
fn machine_env_apply_elevated(ops: &[(&str, Option<&str>)]) -> Result<(), String> {
    run_elevated_script(&machine_env_script(ops), "无法修改系统 PATH")
}

/// 以管理员身份隐藏运行一段 PowerShell 脚本；`cancel_what` 用在 UAC 被取消时的提示里。
fn run_elevated_script(script: &str, cancel_what: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let script_path = std::env::temp_dir().join(format!("ai_workbench_elevated_{}.ps1", std::process::id()));
        fs::write(&script_path, script)
            .map_err(|e| format!("Failed to write elevate script: {}", e))?;
        let script_arg = script_path.to_string_lossy().replace('\'', "''");
        let launcher = format!(
            "$p = Start-Process -FilePath powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList @('-NoProfile','-NonInteractive','-WindowStyle','Hidden','-ExecutionPolicy','Bypass','-File','{script_arg}'); if ($null -eq $p) {{ exit 1223 }}; exit $p.ExitCode"
        );
        let out = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
            .output()
            .map_err(|e| format!("Failed to elevate: {}", e));
        let _ = fs::remove_file(&script_path);
        let output = out?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            let code = output.status.code().unwrap_or(-1);
            if code == 1223 || err.contains("canceled") || err.contains("cancelled") {
                return Err(format!("已取消管理员授权，{}", cancel_what));
            }
            return Err(format!("管理员操作失败（exit {}）{}", code, err.trim()));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (script, cancel_what);
        Err("Runtime switching is only supported on Windows".to_string())
    }
}

fn merged_path_parts(snap: &EnvSnapshot) -> Vec<String> {
    let mut parts = Vec::new();
    parts.extend(split_path(&snap.machine_path));
    parts.extend(split_path(&snap.user_path));
    if parts.is_empty() {
        if let Ok(proc) = std::env::var("PATH") {
            parts.extend(split_path(&proc));
        }
    }
    parts
}

/// PATH 去掉所有已知运行时 bin，其余条目保持原顺序。
fn strip_known_parts(existing: &str, known: &HashSet<String>) -> Vec<String> {
    split_path(existing)
        .into_iter()
        .filter(|p| !known.contains(&path_key(p)))
        .collect()
}

fn strip_known(existing: &str, known: &HashSet<String>) -> String {
    join_path(&strip_known_parts(existing, known))
}

/// Known runtimes out, `inserts` (in order) at the front.
fn rewrite_path(existing: &str, known: &HashSet<String>, inserts: &[String]) -> String {
    let mut parts = strip_known_parts(existing, known);
    parts.retain(|p| !inserts.iter().any(|i| path_eq(p, i)));
    for (i, entry) in inserts.iter().enumerate() {
        parts.insert(i, entry.clone());
    }
    join_path(&parts)
}

fn machine_bins_set(snap: &EnvSnapshot) -> HashSet<String> {
    split_path(&snap.machine_path)
        .into_iter()
        .map(|p| path_key(&p))
        .collect()
}

fn collect_known_bins(list: &[RuntimeVersion], kind: &RuntimeKind) -> HashSet<String> {
    list.iter()
        .filter(|r| r.kind == kind.as_str())
        .flat_map(|r| kind.path_entries(&r.bin_path))
        .map(|p| path_key(&p))
        .collect()
}

/// Oracle 安装器往 PATH 里塞的转发目录（`…\Oracle\Java\javapath`，里面的 java.exe 指向
/// 某个旧 JRE）。它不是一个可选的 JDK，扫描不到，但排在系统 PATH 里就会盖过用户级的选择。
fn is_java_shim_dir(p: &str) -> bool {
    path_key(p).ends_with(r"\oracle\java\javapath")
}

/// 切换时要从 PATH 里摘掉的条目：列表里的全部安装，JDK 再加上 Oracle 转发目录。
fn known_runtime_bins(kind: &RuntimeKind, list: &[RuntimeVersion], snap: &EnvSnapshot) -> HashSet<String> {
    match kind {
        RuntimeKind::Node | RuntimeKind::Maven | RuntimeKind::Python => collect_known_bins(list, kind),
        RuntimeKind::Jdk => {
            let mut known = collect_known_bins(list, kind);
            known.extend(
                split_path(&snap.effective_path())
                    .iter()
                    .filter(|p| is_java_shim_dir(p))
                    .map(|p| path_key(p)),
            );
            known
        }
    }
}

fn mark_active(list: &mut [RuntimeVersion], kind: &RuntimeKind, snap: &EnvSnapshot) {
    let machine_set = machine_bins_set(snap);
    for item in list.iter_mut() {
        item.on_machine_path = machine_set.contains(&path_key(&item.bin_path));
    }

    match kind {
        // mvn.cmd derives MAVEN_HOME from its own location and python.exe has no HOME
        // variable to consult, so PATH order alone decides.
        RuntimeKind::Maven | RuntimeKind::Python => {
            let kind_s = kind.as_str();
            let active_bin = merged_path_parts(snap).into_iter().map(|p| path_key(&p)).find(|key| {
                list.iter().any(|r| r.kind == kind_s && path_key(&r.bin_path) == *key)
            });
            for item in list.iter_mut().filter(|r| r.kind == kind_s) {
                item.active = active_bin.as_deref() == Some(path_key(&item.bin_path).as_str());
            }
        }
        RuntimeKind::Node => {
            let mut active_bin: Option<String> = None;
            for part in merged_path_parts(snap) {
                let key = path_key(&part);
                if list
                    .iter()
                    .any(|r| r.kind == "node" && path_key(&r.bin_path) == key)
                {
                    active_bin = Some(key);
                    break;
                }
            }
            if active_bin.is_none() {
                if let Ok(output) = hidden_cmd("cmd").args(["/c", "where", "node"]).output() {
                    if output.status.success() {
                        if let Some(first) = String::from_utf8_lossy(&output.stdout).lines().next() {
                            if let Some(parent) = Path::new(first.trim()).parent() {
                                active_bin = Some(path_key(&parent.to_string_lossy()));
                            }
                        }
                    }
                }
            }
            for item in list.iter_mut().filter(|r| r.kind == "node") {
                item.active = active_bin
                    .as_ref()
                    .map(|b| path_key(&item.bin_path) == *b)
                    .unwrap_or(false);
            }
        }
        RuntimeKind::Jdk => {
            let java_home = if !snap.user_java_home.is_empty() {
                snap.user_java_home.clone()
            } else if !snap.machine_java_home.is_empty() {
                snap.machine_java_home.clone()
            } else {
                std::env::var("JAVA_HOME").unwrap_or_default()
            };

            let mut matched = false;
            if !java_home.is_empty() {
                for item in list.iter_mut().filter(|r| r.kind == "jdk") {
                    item.active = path_eq(&item.path, &java_home);
                    if item.active {
                        matched = true;
                    }
                }
            }
            if !matched {
                for part in merged_path_parts(snap) {
                    let key = path_key(&part);
                    if let Some(item) = list
                        .iter_mut()
                        .find(|r| r.kind == "jdk" && path_key(&r.bin_path) == key)
                    {
                        item.active = true;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                if let Ok(output) = hidden_cmd("cmd").args(["/c", "where", "java"]).output() {
                    if output.status.success() {
                        if let Some(first) = String::from_utf8_lossy(&output.stdout).lines().next() {
                            if let Some(bin) = Path::new(first.trim()).parent() {
                                if let Some(home) = bin.parent() {
                                    let home_s = normalize_path(home);
                                    for item in list.iter_mut().filter(|r| r.kind == "jdk") {
                                        item.active = path_eq(&item.path, &home_s);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn make_item(
    kind: &str,
    version: String,
    path: String,
    bin_path: String,
    source: String,
    custom: bool,
) -> RuntimeVersion {
    RuntimeVersion {
        kind: kind.into(),
        version,
        path,
        bin_path,
        source,
        active: false,
        custom,
        on_machine_path: false,
    }
}

fn build_list(kind: &RuntimeKind) -> Result<Vec<RuntimeVersion>, String> {
    let snap = load_env_snapshot();
    let customs = load_customs();
    let mut list: Vec<RuntimeVersion> = Vec::new();

    match kind {
        RuntimeKind::Node => {
            for (root, bin, source) in scan_node_dirs(&snap) {
                let version = read_node_version(&bin).unwrap_or_else(|| "?".to_string());
                push_unique(
                    &mut list,
                    make_item(
                        "node",
                        version,
                        normalize_path(&root),
                        normalize_path(&bin),
                        source,
                        false,
                    ),
                );
            }
            for custom in &customs.node {
                let p = PathBuf::from(custom);
                let (root, bin) = if is_node_bin(&p) {
                    (p.clone(), p.clone())
                } else if is_node_bin(&p.join("bin")) {
                    (p.clone(), p.join("bin"))
                } else {
                    continue;
                };
                let version = read_node_version(&bin).unwrap_or_else(|| "?".to_string());
                push_unique(
                    &mut list,
                    make_item(
                        "node",
                        version,
                        normalize_path(&root),
                        normalize_path(&bin),
                        "custom".into(),
                        true,
                    ),
                );
            }
        }
        RuntimeKind::Jdk => {
            for (home, source) in scan_jdk_dirs(&snap) {
                let version = read_jdk_version(&home).unwrap_or_else(|| "?".to_string());
                push_unique(
                    &mut list,
                    make_item(
                        "jdk",
                        version,
                        normalize_path(&home),
                        normalize_path(&home.join("bin")),
                        source,
                        false,
                    ),
                );
            }
            for custom in &customs.jdk {
                let home = PathBuf::from(custom);
                if !is_jdk_home(&home) {
                    continue;
                }
                let version = read_jdk_version(&home).unwrap_or_else(|| "?".to_string());
                push_unique(
                    &mut list,
                    make_item(
                        "jdk",
                        version,
                        normalize_path(&home),
                        normalize_path(&home.join("bin")),
                        "custom".into(),
                        true,
                    ),
                );
            }
        }
        RuntimeKind::Maven => {
            let customs_iter = customs.maven.iter().map(|c| (PathBuf::from(c), "custom".to_string()));
            for (home, source) in scan_maven_dirs(&snap).into_iter().chain(customs_iter) {
                if !is_maven_home(&home) {
                    continue;
                }
                let custom = source == "custom";
                let version = read_maven_version(&home).unwrap_or_else(|| "?".to_string());
                push_unique(
                    &mut list,
                    make_item(
                        "maven",
                        version,
                        normalize_path(&home),
                        normalize_path(&home.join("bin")),
                        source,
                        custom,
                    ),
                );
            }
        }
        // python.exe lives in the install root, so `bin_path` == `path`.
        RuntimeKind::Python => {
            let customs_iter = customs.python.iter().map(|c| (PathBuf::from(c), "custom".to_string()));
            for (home, source) in scan_python_dirs(&snap).into_iter().chain(customs_iter) {
                if !is_python_home(&home) {
                    continue;
                }
                let custom = source == "custom";
                let version = read_python_version(&home).unwrap_or_else(|| "?".to_string());
                let norm = normalize_path(&home);
                push_unique(&mut list, make_item("python", version, norm.clone(), norm, source, custom));
            }
        }
    }

    list.sort_by(|a, b| {
        compare_versions(&b.version, &a.version).then_with(|| a.path.cmp(&b.path))
    });
    mark_active(&mut list, kind, &snap);
    list.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| compare_versions(&b.version, &a.version))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(list)
}

fn compute_switch_plan(
    kind: &RuntimeKind,
    target: &RuntimeVersion,
    list: &[RuntimeVersion],
    snap: &EnvSnapshot,
) -> SwitchPlan {
    let known = known_runtime_bins(kind, list, snap);

    let first_machine_known = split_path(&snap.machine_path)
        .into_iter()
        .find(|p| known.contains(&path_key(p)));

    let needs = match &first_machine_known {
        Some(existing) => !path_eq(existing, &target.bin_path),
        None => false,
    };

    SwitchPlan {
        needs_elevation: needs,
        reason: if needs {
            Some("系统 PATH 中有其他运行时条目，需管理员授权一次，把它们移到用户 PATH，之后切换免授权".to_string())
        } else {
            None
        },
    }
}

/// 返回（是否命中目标, 实际版本, 新终端实际会用到的目录）。
fn verify_resolved(kind: &RuntimeKind, expected_bin: &str) -> (bool, Option<String>, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        let (file, ver_args) = match kind {
            RuntimeKind::Node => ("node.exe", "-v"),
            RuntimeKind::Jdk => ("java.exe", "-version"),
            RuntimeKind::Maven => ("mvn.cmd", ""),
            RuntimeKind::Python => ("python.exe", "-V"),
        };
        // 注册表里的新值（不是本进程继承的旧环境），按新进程看到的顺序：系统在前、用户在后。
        let snap = load_env_snapshot();
        let Some(found) = split_path(&snap.effective_path())
            .into_iter()
            .map(|dir| Path::new(&dir).join(file))
            .find(|p| p.is_file())
        else {
            return (false, None, None);
        };
        let found_dir = found.parent().map(normalize_path).unwrap_or_default();
        let path_ok = !found_dir.is_empty() && path_eq(&found_dir, &normalize_path(Path::new(expected_bin)));

        let version = match kind {
            RuntimeKind::Maven => found.parent().and_then(Path::parent).and_then(read_maven_version),
            _ => run_capture(&found, &[ver_args]).and_then(|out| match kind {
                RuntimeKind::Jdk => parse_java_version(&out),
                RuntimeKind::Python => parse_python_version(&out),
                _ => {
                    let v = out.lines().next().unwrap_or("").trim().trim_start_matches('v');
                    (!v.is_empty()).then(|| v.to_string())
                }
            }),
        };

        (path_ok, version, (!found_dir.is_empty()).then_some(found_dir))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (kind, expected_bin);
        (false, None, None)
    }
}

#[tauri::command]
pub async fn list_runtime_versions(kind: String) -> Result<Vec<RuntimeVersion>, String> {
    tokio::task::spawn_blocking(move || {
        let kind = RuntimeKind::from_str(&kind)?;
        build_list(&kind)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn get_active_runtime(kind: String) -> Result<Option<RuntimeVersion>, String> {
    tokio::task::spawn_blocking(move || {
        let kind = RuntimeKind::from_str(&kind)?;
        let list = build_list(&kind)?;
        Ok(list.into_iter().find(|r| r.active))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn plan_runtime_switch(kind: String, path: String) -> Result<SwitchPlan, String> {
    tokio::task::spawn_blocking(move || {
        let kind = RuntimeKind::from_str(&kind)?;
        let snap = load_env_snapshot();
        let list = build_list(&kind)?;
        let target = list
            .iter()
            .find(|r| path_eq(&r.path, &path) || path_eq(&r.bin_path, &path))
            .ok_or_else(|| "未找到该运行时，请先刷新或手动添加".to_string())?;
        Ok(compute_switch_plan(&kind, target, &list, &snap))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

fn add_custom_runtime_sync(kind: String, path: String) -> Result<RuntimeVersion, String> {
    let kind = RuntimeKind::from_str(&kind)?;
    let p = PathBuf::from(path.trim());
    if !p.exists() {
        return Err("路径不存在".to_string());
    }

    let mut customs = load_customs();
    match kind {
        RuntimeKind::Node => {
            let bin = if is_node_bin(&p) {
                p.clone()
            } else if is_node_bin(&p.join("bin")) {
                p.join("bin")
            } else {
                return Err("未在该目录找到 node.exe".to_string());
            };
            let norm = normalize_path(&p);
            if !customs.node.iter().any(|x| path_eq(x, &norm)) {
                customs.node.push(norm.clone());
                save_customs(&customs)?;
            }
            let version = read_node_version(&bin).unwrap_or_else(|| "?".to_string());
            Ok(make_item(
                "node",
                version,
                norm,
                normalize_path(&bin),
                "custom".into(),
                true,
            ))
        }
        RuntimeKind::Jdk => {
            let home = if is_jdk_home(&p) {
                p.clone()
            } else if is_jdk_home(p.parent().unwrap_or(&p)) {
                p.parent().unwrap().to_path_buf()
            } else {
                return Err("未在该目录找到有效的 JDK（缺少 bin/java.exe）".to_string());
            };
            let norm = normalize_path(&home);
            if !customs.jdk.iter().any(|x| path_eq(x, &norm)) {
                customs.jdk.push(norm.clone());
                save_customs(&customs)?;
            }
            let version = read_jdk_version(&home).unwrap_or_else(|| "?".to_string());
            Ok(make_item(
                "jdk",
                version,
                norm,
                normalize_path(&home.join("bin")),
                "custom".into(),
                true,
            ))
        }
        RuntimeKind::Maven => {
            let home = if is_maven_home(&p) {
                p.clone()
            } else if p.parent().is_some_and(is_maven_home) {
                p.parent().unwrap().to_path_buf()
            } else {
                return Err("未在该目录找到有效的 Maven（缺少 bin/mvn.cmd）".to_string());
            };
            let norm = normalize_path(&home);
            if !customs.maven.iter().any(|x| path_eq(x, &norm)) {
                customs.maven.push(norm.clone());
                save_customs(&customs)?;
            }
            let version = read_maven_version(&home).unwrap_or_else(|| "?".to_string());
            Ok(make_item(
                "maven",
                version,
                norm,
                normalize_path(&home.join("bin")),
                "custom".into(),
                true,
            ))
        }
        RuntimeKind::Python => {
            // Accept the install root, or its `Scripts` folder picked by mistake.
            let home = if is_python_home(&p) {
                p.clone()
            } else if p.parent().is_some_and(is_python_home) {
                p.parent().unwrap().to_path_buf()
            } else {
                return Err("未在该目录找到有效的 Python（缺少 python.exe 或 Lib\\os.py）".to_string());
            };
            let norm = normalize_path(&home);
            if !customs.python.iter().any(|x| path_eq(x, &norm)) {
                customs.python.push(norm.clone());
                save_customs(&customs)?;
            }
            let version = read_python_version(&home).unwrap_or_else(|| "?".to_string());
            Ok(make_item("python", version, norm.clone(), norm, "custom".into(), true))
        }
    }
}

#[tauri::command]
pub async fn add_custom_runtime(kind: String, path: String) -> Result<RuntimeVersion, String> {
    tokio::task::spawn_blocking(move || add_custom_runtime_sync(kind, path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub fn remove_custom_runtime(kind: String, path: String) -> Result<(), String> {
    let kind = RuntimeKind::from_str(&kind)?;
    let mut customs = load_customs();
    customs.paths_mut(&kind).retain(|x| !path_eq(x, &path));
    save_customs(&customs)
}

/// Open a terminal that uses the *current* registry environment, so a runtime switch
/// can be confirmed (and used) without restarting the IDE. Passing `bin_path` previews
/// one specific install even before switching to it.
fn open_runtime_terminal_sync(kind: RuntimeKind, bin_path: Option<String>) -> Result<(), String> {
    let snap = load_env_snapshot();
    // Registry values, never this process's env: the app may predate the switch.
    let mut path_value = snap.effective_path();
    let mut java_home = if snap.user_java_home.trim().is_empty() {
        snap.machine_java_home.clone()
    } else {
        snap.user_java_home.clone()
    };
    let mut maven_home: Option<String> = None;

    if let Some(bin) = bin_path.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) {
        let bin = PathBuf::from(normalize_path(Path::new(&bin)));
        if bin.is_dir() {
            let mut entries = kind.path_entries(&bin.to_string_lossy());
            if !path_value.trim().is_empty() {
                entries.push(path_value);
            }
            path_value = join_path(&entries);
            if let Some(home) = bin.parent() {
                match kind {
                    RuntimeKind::Jdk => java_home = home.to_string_lossy().to_string(),
                    RuntimeKind::Maven => maven_home = Some(home.to_string_lossy().to_string()),
                    RuntimeKind::Node | RuntimeKind::Python => {}
                }
            }
        }
    }

    let probe = match kind {
        RuntimeKind::Node => "node -v",
        RuntimeKind::Jdk => "java -version",
        RuntimeKind::Maven => "mvn -v",
        RuntimeKind::Python => "python -V && pip -V",
    };
    // Visible window on purpose here (unlike the probe helpers above).
    let mut cmd = Command::new("cmd");
    cmd.args(["/k", probe]);
    if !path_value.trim().is_empty() {
        cmd.env("PATH", &path_value);
    }
    if !java_home.trim().is_empty() {
        cmd.env("JAVA_HOME", &java_home);
    }
    if let Some(home) = maven_home {
        cmd.env("MAVEN_HOME", &home).env("M2_HOME", &home);
    }
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NEW_CONSOLE);
    cmd.spawn()
        .map_err(|e| format!("无法打开终端: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn open_runtime_terminal(
    kind: String,
    bin_path: Option<String>,
) -> Result<(), String> {
    let kind = RuntimeKind::from_str(&kind)?;
    tokio::task::spawn_blocking(move || open_runtime_terminal_sync(kind, bin_path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub fn open_runtime_folder(path: String) -> Result<(), String> {
    // explorer 不认正斜杠，`D:/a/b` 会直接打开默认的「文档」而不报错
    let p = PathBuf::from(path.trim().replace('/', "\\"));
    if !p.exists() {
        return Err("路径不存在".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        hidden_cmd("explorer")
            .arg(p.as_os_str())
            .spawn()
            .map_err(|e| format!("无法打开目录: {}", e))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("仅支持 Windows".to_string())
    }
}

fn switch_runtime_sync(kind: String, path: String) -> Result<SwitchResult, String> {
    let kind = RuntimeKind::from_str(&kind)?;
    let snap = load_env_snapshot();
    let list = build_list(&kind)?;
    let target = list
        .iter()
        .find(|r| path_eq(&r.path, &path) || path_eq(&r.bin_path, &path))
        .cloned()
        .ok_or_else(|| "未找到该运行时，请先刷新或手动添加".to_string())?;

    let known = known_runtime_bins(&kind, &list, &snap);

    let plan = compute_switch_plan(&kind, &target, &list, &snap);
    let mut elevated = false;
    let mut machine_error: Option<String> = None;

    // Machine scope first: it is the only step that can be aborted (UAC prompt).
    // Writing the User scope first left a half-switched environment whenever the
    // prompt was cancelled — User PATH already rewritten, Machine untouched — and
    // the caller could only report that as a plain failure.
    //
    // 提权时不再把目标写进系统 PATH，而是把已知运行时从系统 PATH 摘掉（顺带删系统
    // JAVA_HOME，用户级同名变量本来就会覆盖它），一次 UAC 搞定；之后系统 PATH 里没有
    // 运行时条目，再切换只写用户级，免授权。
    if plan.needs_elevation {
        let new_machine = strip_known(&snap.machine_path, &known);
        let mut ops: Vec<(&str, Option<&str>)> = vec![("Path", Some(new_machine.as_str()))];
        if matches!(kind, RuntimeKind::Jdk) && !snap.machine_java_home.trim().is_empty() {
            ops.push(("JAVA_HOME", None));
        }
        if matches!(kind, RuntimeKind::Maven) {
            // Removing an absent value is a no-op in the elevated script.
            ops.push(("MAVEN_HOME", None));
            ops.push(("M2_HOME", None));
        }
        match machine_env_apply_elevated(&ops) {
            Ok(()) => elevated = true,
            Err(e) => machine_error = Some(e),
        }
    }

    if let Some(err) = machine_error {
        // The User scope is deliberately untouched, so the environment stays consistent.
        return Ok(SwitchResult {
            message: format!("未修改任何环境变量：{}", err),
            version: target.version,
            verified: false,
            verified_version: None,
            elevated,
            list,
        });
    }

    // User scope last: these writes cannot prompt, and the machine scope is
    // already committed, so a failure here must not read as a no-op.
    let mut user_error: Option<String> = None;
    let new_user = rewrite_path(&snap.user_path, &known, &kind.path_entries(&target.bin_path));
    if let Err(e) = user_env_set("Path", &new_user) {
        user_error = Some(e);
    } else if matches!(kind, RuntimeKind::Jdk) {
        if let Err(e) = user_env_set("JAVA_HOME", &target.path) {
            user_error = Some(e);
        }
    } else if matches!(kind, RuntimeKind::Maven) {
        // M2_HOME is the Maven 2 name; only keep it in sync if the user already has one,
        // since a stale value still steers older IDE integrations to the wrong install.
        let mut names = vec!["MAVEN_HOME"];
        if user_env_exists("M2_HOME") {
            names.push("M2_HOME");
        }
        for name in names {
            if let Err(e) = user_env_set(name, &target.path) {
                user_error = Some(e);
                break;
            }
        }
    }

    if user_error.is_none() {
        // One broadcast for the whole switch — `env_set` used to notify on every write.
        broadcast_env_change();
    }

    let (mut verified, verified_version, resolved_dir) = verify_resolved(&kind, &target.bin_path);
    if user_error.is_some() {
        verified = false;
    }

    let label = match kind {
        RuntimeKind::Node => format!("Node.js v{}", target.version),
        RuntimeKind::Jdk => format!("JDK {}", target.version),
        RuntimeKind::Maven => format!("Maven {}", target.version),
        RuntimeKind::Python => format!("Python {}", target.version),
    };
    let mut message = if let Some(err) = user_error.as_deref() {
        if elevated {
            format!("系统环境变量已更新，但用户环境变量写入失败：{}。请重试一次", err)
        } else {
            format!("用户环境变量写入失败：{}", err)
        }
    } else if verified {
        format!("已切换至 {}（新开终端生效）", label)
    } else if let Some(dir) = resolved_dir.as_deref() {
        format!(
            "已写入环境变量（{}），但新终端会先找到 {}{}，请把它从 PATH 中移除",
            label,
            dir,
            verified_version.as_deref().map(|v| format!("（版本 {}）", v)).unwrap_or_default()
        )
    } else {
        format!("已写入环境变量（{}），但在 PATH 中找不到可执行文件，请确认 PATH", label)
    };
    if elevated && user_error.is_none() {
        message.push_str("；已把运行时从系统 PATH 移到用户 PATH，之后切换免授权");
    }
    if verified {
        if let Some(ref v) = verified_version {
            message.push_str(&format!("；校验版本 {}", v));
        }
    }

    // 切换后的列表直接带回去，前端不必再全量 build_list 一次。
    let list = build_list(&kind).unwrap_or(list);
    Ok(SwitchResult {
        message,
        version: target.version,
        verified,
        verified_version,
        elevated,
        list,
    })
}

#[tauri::command]
pub async fn switch_runtime(kind: String, path: String) -> Result<SwitchResult, String> {
    tokio::task::spawn_blocking(move || switch_runtime_sync(kind, path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

// ---------------------------------------------------------------------------
// Installing new versions
//
// Downloads land in <appdata>/runtimes/{node|jdk}/<version> and are picked up by
// scan_node_dirs / scan_jdk_dirs as source "managed". The archive URL is derived
// here from the validated version, so the frontend never supplies a URL.
// ---------------------------------------------------------------------------

const NODE_INDEX_URL: &str = "https://nodejs.org/dist/index.json";
const ADOPTIUM_AVAILABLE_URL: &str = "https://api.adoptium.net/v3/info/available_releases";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallableVersion {
    pub version: String,
    pub lts: bool,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    stage: String,
    percent: u8,
    message: String,
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("ai-workbench")
        .build()
        .map_err(|e| format!("初始化网络客户端失败: {}", e))
}

async fn fetch_json(url: &str) -> Result<serde_json::Value, String> {
    let resp = http_client(20)?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("请求失败: HTTP {}", resp.status()));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("解析响应失败: {}", e))
}

/// Only values that are safe in both a URL path and a directory name.
fn validate_version(version: &str) -> Result<(), String> {
    let ok = !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if ok {
        Ok(())
    } else {
        Err(format!("版本号不合法: {}", version))
    }
}

/// Download sources, official host first. The mirrors matter in practice: the
/// Node.js host and the GitHub release assets Adoptium redirects to are often slow
/// or unreachable from mainland China.
fn archive_urls(kind: &RuntimeKind, version: &str) -> Result<Vec<String>, String> {
    validate_version(version)?;
    Ok(match kind {
        RuntimeKind::Node => vec![
            format!("https://nodejs.org/dist/v{version}/node-v{version}-win-x64.zip"),
            format!(
                "https://registry.npmmirror.com/-/binary/node/v{version}/node-v{version}-win-x64.zip"
            ),
            format!(
                "https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/v{version}/node-v{version}-win-x64.zip"
            ),
        ],
        RuntimeKind::Jdk => {
            // "jdk-21.0.12.1+1" → "OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip"
            let semver = version.strip_prefix("jdk-").unwrap_or(version);
            // `jdk8u462-b08` has no `jdk-` prefix; it must still map to major 8.
            let major = if version.starts_with("jdk8u") {
                "8".to_string()
            } else {
                semver.split('.').next().unwrap_or_default().to_string()
            };
            let mut urls = vec![format!(
                "https://api.adoptium.net/v3/binary/version/{}/windows/x64/jdk/hotspot/normal/eclipse?project=jdk",
                version.replace('+', "%2B")
            )];
            // JDK 8 uses a different mirror naming scheme, so it stays official-only.
            if major != "8" {
                urls.push(format!(
                    "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/{major}/jdk/x64/windows/OpenJDK{major}U-jdk_x64_windows_hotspot_{}.zip",
                    semver.replace('+', "_")
                ));
            }
            urls
        }
        // Maven Central keeps every release; Aliyun mirrors it and is reliably fast in China.
        RuntimeKind::Maven => {
            let file = format!("org/apache/maven/apache-maven/{version}/apache-maven-{version}-bin.zip");
            let major = version.split('.').next().unwrap_or("3");
            vec![
                format!("https://repo.maven.apache.org/maven2/{file}"),
                format!("https://maven.aliyun.com/repository/public/{file}"),
                format!(
                    "https://archive.apache.org/dist/maven/maven-{major}/{version}/binaries/apache-maven-{version}-bin.zip"
                ),
            ]
        }
        // python.org only ships installers and the stdlib-less "embeddable" zip; the NuGet
        // `python` package is the full interpreter (pip included) as a plain zip.
        RuntimeKind::Python => vec![
            format!("https://api.nuget.org/v3-flatcontainer/python/{version}/python.{version}.nupkg"),
            format!("https://www.nuget.org/api/v2/package/python/{version}"),
        ],
    })
}

/// Sub-directory of the extracted archive that is the actual install.
fn archive_payload_dir(kind: &RuntimeKind) -> Option<&'static str> {
    match kind {
        RuntimeKind::Python => Some("tools"),
        _ => None,
    }
}

fn source_host(url: &str) -> String {
    url.split('/').nth(2).unwrap_or(url).to_string()
}

/// Stream one source to `archive`, reporting progress. Gives up after 30s without
/// data so a stalled host falls through to the next source instead of hanging.
async fn download_archive(
    url: &str,
    archive: &Path,
    window: &tauri::Window,
) -> Result<(), String> {
    let mut resp = http_client(1800)?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("连接失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(archive)
        .await
        .map_err(|e| format!("创建文件失败: {}", e))?;
    let mut downloaded: u64 = 0;
    let mut reported: u64 = 0;
    loop {
        let chunk = match tokio::time::timeout(Duration::from_secs(30), resp.chunk()).await {
            Err(_) => return Err("下载超时（30 秒无数据）".to_string()),
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(format!("下载中断: {}", e)),
        };
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入失败: {}", e))?;
        downloaded += chunk.len() as u64;
        if downloaded - reported >= 1_048_576 {
            reported = downloaded;
            let percent = (downloaded * 100).checked_div(total).map(|p| p.min(99) as u8).unwrap_or(0);
            emit_progress(
                window,
                "download",
                percent,
                &format!("已下载 {}", format_size(downloaded)),
            );
        }
    }
    file.flush().await.map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}

fn managed_runtimes_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("runtimes"))
}

fn managed_kind_dir(kind: &RuntimeKind) -> Result<PathBuf, String> {
    Ok(managed_runtimes_dir()?.join(kind.as_str()))
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.0} KB", value / KB)
    }
}

fn emit_progress(window: &tauri::Window, stage: &str, percent: u8, message: &str) {
    let _ = window.emit(
        "runtime:install_progress",
        InstallProgress {
            stage: stage.to_string(),
            percent,
            message: message.to_string(),
        },
    );
}

/// Newest patch release per major version, LTS lines flagged. Node's index lists
/// every patch ever published; showing all 584 of them is useless, and the newest
/// patch of each major is what people actually pick.
async fn list_installable_node(installed: &[RuntimeVersion]) -> Result<Vec<InstallableVersion>, String> {
    let data = fetch_json(NODE_INDEX_URL).await?;
    let entries = data
        .as_array()
        .ok_or_else(|| "版本列表格式异常".to_string())?;

    let mut seen_major: HashSet<u64> = HashSet::new();
    let mut out = Vec::new();
    for entry in entries {
        let version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim_start_matches('v')
            .to_string();
        let has_zip = entry
            .get("files")
            .and_then(|v| v.as_array())
            .map(|files| files.iter().any(|f| f.as_str() == Some("win-x64-zip")))
            .unwrap_or(false);
        if version.is_empty() || !has_zip {
            continue;
        }
        let major = version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(0);
        if !seen_major.insert(major) {
            continue;
        }
        // `lts` is either false or the codename string ("Iron").
        let lts = entry
            .get("lts")
            .map(|v| !v.is_null() && v.as_bool().unwrap_or(true))
            .unwrap_or(false);
        out.push(InstallableVersion {
            installed: installed.iter().any(|i| i.version == version),
            version,
            lts,
        });
        if out.len() >= 40 {
            break;
        }
    }
    if out.is_empty() {
        return Err("Node.js 发布列表为空".to_string());
    }
    Ok(out)
}

/// Version strings a JDK release name can show up as after install
/// (`release` file / `java -version`): `jdk-21.0.2+13` → `21.0.2`, `21.0.2+13`;
/// `jdk8u462-b08` → `1.8.0_462`.
fn jdk_installed_aliases(name: &str) -> Vec<String> {
    if let Some(rest) = name.strip_prefix("jdk-") {
        let semver = rest.split('+').next().unwrap_or(rest).to_string();
        return vec![semver, rest.to_string()];
    }
    if let Some(rest) = name.strip_prefix("jdk8u") {
        let update = rest.split('-').next().unwrap_or(rest);
        return vec![format!("1.8.0_{}", update)];
    }
    vec![name.to_string()]
}

/// Newest GA build of every major Adoptium still serves. `release_names` caps a page
/// at 20 names, which a handful of recent majors (each with several patches) used up
/// on their own, hiding 8/11/17/21 entirely — so ask per major instead.
async fn list_installable_jdk(installed: &[RuntimeVersion]) -> Result<Vec<InstallableVersion>, String> {
    let info = fetch_json(ADOPTIUM_AVAILABLE_URL).await?;
    let majors = |key: &str| -> Vec<u64> {
        info.get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|m| m.as_u64()).collect())
            .unwrap_or_default()
    };
    let lts: HashSet<u64> = majors("available_lts_releases").into_iter().collect();
    let mut all = majors("available_releases");
    if all.is_empty() {
        return Err("JDK 发布列表为空".to_string());
    }
    all.sort_unstable_by(|a, b| b.cmp(a));

    let lookups = all.iter().map(|major| async move {
        let url = format!(
            "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse"
        );
        let data = fetch_json(&url).await.ok()?;
        let name = data.as_array()?.first()?.get("release_name")?.as_str()?.to_string();
        Some((*major, name))
    });
    let mut out = Vec::new();
    for (major, name) in futures_util::future::join_all(lookups).await.into_iter().flatten() {
        let aliases = jdk_installed_aliases(&name);
        out.push(InstallableVersion {
            installed: installed.iter().any(|i| aliases.contains(&i.version)),
            version: name,
            lts: lts.contains(&major),
        });
    }
    if out.is_empty() {
        return Err("JDK 发布列表为空".to_string());
    }
    Ok(out)
}

const MAVEN_METADATA_URLS: [&str; 2] = [
    "https://repo.maven.apache.org/maven2/org/apache/maven/apache-maven/maven-metadata.xml",
    "https://maven.aliyun.com/repository/public/org/apache/maven/apache-maven/maven-metadata.xml",
];

/// GA versions (3.0+) from `maven-metadata.xml`, newest first. Pre-releases
/// (`4.0.0-rc-4`, `-beta-`, `-alpha-`) are dropped: they don't belong in a tool switcher.
fn parse_maven_versions(xml: &str) -> Vec<String> {
    let mut versions: Vec<String> = xml
        .split("<version>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</version>").next())
        .map(|v| v.trim().to_string())
        .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .filter(|v| v.split('.').next().and_then(|m| m.parse::<u64>().ok()).is_some_and(|m| m >= 3))
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions.dedup();
    versions
}

async fn list_installable_maven(installed: &[RuntimeVersion]) -> Result<Vec<InstallableVersion>, String> {
    let mut last_err = String::new();
    for url in MAVEN_METADATA_URLS {
        let text = match http_client(20)?.get(url).send().await {
            Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
            Ok(resp) => {
                last_err = format!("HTTP {} [{}]", resp.status(), source_host(url));
                continue;
            }
            Err(e) => {
                last_err = format!("{} [{}]", e, source_host(url));
                continue;
            }
        };
        let versions = parse_maven_versions(&text);
        if versions.is_empty() {
            last_err = format!("版本列表为空 [{}]", source_host(url));
            continue;
        }
        return Ok(versions
            .into_iter()
            .map(|version| InstallableVersion {
                installed: installed.iter().any(|i| i.version == version),
                version,
                lts: false,
            })
            .collect());
    }
    Err(format!("获取 Maven 版本列表失败: {}", last_err))
}

const PYTHON_NUGET_INDEX_URL: &str = "https://api.nuget.org/v3-flatcontainer/python/index.json";

/// GA releases 3.8+ from the NuGet index, newest first (`-a1` / `-b2` / `-rc1` dropped).
fn parse_python_versions(index: &serde_json::Value) -> Vec<String> {
    let mut versions: Vec<String> = index
        .get("versions")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();
    versions.retain(|v| {
        let nums: Vec<u64> = v.split('.').map_while(|p| p.parse().ok()).collect();
        v.chars().all(|c| c.is_ascii_digit() || c == '.') && nums.len() == 3 && (nums[0], nums[1]) >= (3, 8)
    });
    versions.sort_by(|a, b| compare_versions(b, a));
    versions.dedup();
    versions
}

async fn list_installable_python(installed: &[RuntimeVersion]) -> Result<Vec<InstallableVersion>, String> {
    let index = fetch_json(PYTHON_NUGET_INDEX_URL).await?;
    let versions = parse_python_versions(&index);
    if versions.is_empty() {
        return Err("Python 发布列表为空".to_string());
    }
    Ok(versions
        .into_iter()
        .map(|version| InstallableVersion {
            installed: installed.iter().any(|i| i.version == version),
            version,
            lts: false,
        })
        .collect())
}

#[tauri::command]
pub async fn list_installable_runtimes(kind: String) -> Result<Vec<InstallableVersion>, String> {
    let kind = RuntimeKind::from_str(&kind)?;
    let installed = {
        let kind = kind.clone();
        tokio::task::spawn_blocking(move || build_list(&kind))
            .await
            .map_err(|e| format!("Task failed: {}", e))??
    };
    match kind {
        RuntimeKind::Node => list_installable_node(&installed).await,
        RuntimeKind::Jdk => list_installable_jdk(&installed).await,
        RuntimeKind::Maven => list_installable_maven(&installed).await,
        RuntimeKind::Python => list_installable_python(&installed).await,
    }
}

/// bsdtar ships with Windows 10 1803+ and reads zip, and is far faster than
/// Expand-Archive on a multi-hundred-MB JDK. Fall back when it is unavailable.
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let archive_s = archive.to_string_lossy().to_string();
    let dest_s = dest.to_string_lossy().to_string();

    let tar_ok = hidden_cmd("tar")
        .args(["-xf", &archive_s, "-C", &dest_s])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if tar_ok {
        return Ok(());
    }

    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        archive_s.replace('\'', "''"),
        dest_s.replace('\'', "''")
    );
    let output = hidden_cmd("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("解压失败: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "解压失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Archives wrap the payload in a single top-level directory
/// (`node-v22.11.0-win-x64`, `jdk-21.0.12.1+1`); promote it to `dest`.
fn promote_single_dir(temp: &Path, dest: &Path) -> Result<(), String> {
    let entries: Vec<PathBuf> = fs::read_dir(temp)
        .map_err(|e| format!("读取解压目录失败: {}", e))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    let source = match entries.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => temp.to_path_buf(),
    };
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    // Staging and destination share a volume, so a rename avoids copying the payload.
    fs::rename(&source, dest).map_err(|e| format!("移动到安装目录失败: {}", e))
}

#[tauri::command]
pub async fn install_runtime(
    kind: String,
    version: String,
    window: tauri::Window,
) -> Result<RuntimeVersion, String> {
    let kind = RuntimeKind::from_str(&kind)?;
    validate_version(&version)?;

    let urls = archive_urls(&kind, &version)?;
    let dest = managed_kind_dir(&kind)?.join(&version);
    if dest.exists() {
        return Err(format!("{} 已经安装", version));
    }

    let staging = managed_runtimes_dir()?.join(".download");
    fs::create_dir_all(&staging).map_err(|e| format!("创建下载目录失败: {}", e))?;
    let stem = version.replace('+', "_");
    let archive = staging.join(format!("{}.zip", stem));

    emit_progress(&window, "download", 0, "正在下载…");
    let mut failures: Vec<String> = Vec::new();
    let mut fetched = false;
    for (index, url) in urls.iter().enumerate() {
        if index > 0 {
            let _ = fs::remove_file(&archive);
            emit_progress(&window, "download", 0, "正在尝试其它下载源…");
        }
        match download_archive(url, &archive, &window).await {
            Ok(()) => {
                fetched = true;
                break;
            }
            Err(e) => failures.push(format!("{} [{}]", e, source_host(url))),
        }
    }
    if !fetched {
        let _ = fs::remove_file(&archive);
        return Err(format!("下载失败: {}", failures.join("；")));
    }

    emit_progress(&window, "extract", 100, "正在解压…");
    let temp = staging.join(format!("x-{}", stem));
    {
        let archive = archive.clone();
        let temp = temp.clone();
        let dest = dest.clone();
        let payload = archive_payload_dir(&kind);
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let _ = fs::remove_dir_all(&temp);
            fs::create_dir_all(&temp).map_err(|e| format!("创建临时目录失败: {}", e))?;
            extract_zip(&archive, &temp)?;
            match payload {
                Some(sub) => {
                    let source = temp.join(sub);
                    if !source.is_dir() {
                        return Err(format!("安装包结构异常：缺少 {} 目录", sub));
                    }
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
                    }
                    fs::rename(&source, &dest).map_err(|e| format!("移动到安装目录失败: {}", e))?;
                }
                None => promote_single_dir(&temp, &dest)?,
            }
            let _ = fs::remove_dir_all(&temp);
            Ok(())
        })
        .await
        .map_err(|e| format!("Task failed: {}", e))??;
    }
    let _ = fs::remove_file(&archive);

    emit_progress(&window, "done", 100, "安装完成");
    let fresh = {
        let kind = kind.clone();
        tokio::task::spawn_blocking(move || build_list(&kind))
            .await
            .map_err(|e| format!("Task failed: {}", e))??
    };
    fresh
        .into_iter()
        .find(|item| path_eq(&item.path, &dest.to_string_lossy()))
        .ok_or_else(|| format!("已解压到 {}，但未能在版本列表中定位", dest.display()))
}

fn remove_dir_with_retry(path: &Path, attempts: u32) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..attempts {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(300 * (attempt as u64 + 1)));
            }
        }
    }
    Err(format!("删除目录失败: {}", last))
}

/// Directories that must never be deleted, nor any of their ancestors: deleting a
/// runtime must not be able to take a whole profile / Program Files / drive with it.
fn protected_dir_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "SystemDrive", "SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432",
        "ProgramData", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "PUBLIC",
    ]
    .iter()
    .filter_map(|v| std::env::var(v).ok())
    .filter(|v| !v.trim().is_empty())
    .map(|v| path_key(&v))
    .collect();
    if let Ok(dir) = managed_runtimes_dir() {
        keys.push(path_key(&normalize_path(&dir)));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        keys.push(path_key(&normalize_path(&dir)));
    }
    keys
}

/// `target` is a protected dir, a drive root, or contains a protected dir.
fn is_protected_dir(target_key: &str, protected: &[String]) -> bool {
    let is_drive_root = target_key.len() <= 2 || !target_key.contains('\\');
    is_drive_root
        || protected.iter().any(|p| {
            p == target_key || p.starts_with(&format!("{}\\", target_key))
        })
}

/// The directory really is a runtime distribution, not a shared bin folder that merely
/// holds a `node.exe` / `java.exe` shim (`chocolatey\bin`, `scoop\shims`, `javapath`).
fn looks_like_runtime_home(kind: &RuntimeKind, dir: &Path) -> bool {
    match kind {
        RuntimeKind::Node => dir.join("node.exe").is_file() && dir.join("node_modules").join("npm").is_dir(),
        RuntimeKind::Jdk => {
            is_jdk_home(dir)
                && !is_java_shim_dir(&dir.join("bin").to_string_lossy())
                && (dir.join("release").is_file() || dir.join("lib").is_dir())
        }
        RuntimeKind::Maven => is_maven_home(dir) && read_maven_version(dir).is_some(),
        RuntimeKind::Python => is_python_home(dir),
    }
}

fn can_write_into(dir: &Path) -> bool {
    let probe = dir.join(format!(".aiwb-write-probe-{}", std::process::id()));
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// Rename first, then delete: a rename fails up front when any file inside is open,
/// whereas `remove_dir_all` would stop halfway and leave a broken install behind.
fn delete_runtime_dir(target: &Path) -> Result<(), String> {
    let leaf = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let trash_leaf = format!("{leaf}.aiwb-uninstall-{stamp}");
    let trash = target.with_file_name(&trash_leaf);
    match fs::rename(target, &trash) {
        Ok(()) => remove_dir_with_retry(&trash, 3)
            .map_err(|e| format!("已移出原位置，但部分文件未能删除（{}）：{}", e, trash.display())),
        Err(_) if target.parent().is_some_and(can_write_into) => {
            Err("有程序正在使用该目录（IDE、终端或正在运行的服务），请关闭后重试".to_string())
        }
        Err(_) => {
            let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
            let script = format!(
                "$ErrorActionPreference = 'Stop'\nRename-Item -LiteralPath {} -NewName {}\nRemove-Item -LiteralPath {} -Recurse -Force\nexit 0\n",
                q(&target.to_string_lossy()),
                q(&trash_leaf),
                q(&trash.to_string_lossy()),
            );
            run_elevated_script(&script, "未卸载").map_err(|e| {
                if trash.exists() {
                    format!("已移出原位置，但部分文件未能删除：{}", trash.display())
                } else {
                    format!("{e}；如果有程序正在使用该目录，请关闭后重试")
                }
            })
        }
    }
}

/// App-installed versions and, with stricter checks, ones from other sources.
fn uninstall_runtime_sync(kind: RuntimeKind, path: String) -> Result<(), String> {
    let target = PathBuf::from(normalize_path(Path::new(path.trim())));
    if !target.is_dir() {
        return Err("安装目录不存在".to_string());
    }
    let target_s = target.to_string_lossy().to_string();
    let list = build_list(&kind)?;
    let item = list
        .iter()
        .find(|item| path_eq(&item.path, &target_s))
        .cloned()
        .ok_or_else(|| "该目录不在版本列表中，请先刷新".to_string())?;
    if item.active {
        return Err("该版本正在使用中，请先切换到其它版本".to_string());
    }
    if is_protected_dir(&path_key(&target_s), &protected_dir_keys()) {
        return Err(format!("出于安全考虑，不删除该目录：{}", target_s));
    }
    if !looks_like_runtime_home(&kind, &target) {
        return Err("该目录不像一个完整的运行时安装（可能是多个工具共用的目录），为安全起见不删除".to_string());
    }

    delete_runtime_dir(&target)?;

    // Leftovers that would point at a directory that no longer exists.
    let mut customs = load_customs();
    let before = customs.paths_mut(&kind).len();
    customs.paths_mut(&kind).retain(|x| !path_eq(x, &target_s));
    if customs.paths_mut(&kind).len() != before {
        let _ = save_customs(&customs);
    }
    let snap = load_env_snapshot();
    let mut env_changed = false;
    let entries = kind.path_entries(&item.bin_path);
    let is_entry = |p: &String| entries.iter().any(|e| path_eq(p, e));
    if split_path(&snap.user_path).iter().any(is_entry) {
        let kept: Vec<String> = split_path(&snap.user_path).into_iter().filter(|p| !is_entry(p)).collect();
        env_changed |= user_env_set("Path", &join_path(&kept)).is_ok();
    }
    if matches!(kind, RuntimeKind::Maven) {
        #[cfg(target_os = "windows")]
        if let Some(user) = reg_query_env(r"HKCU\Environment") {
            for name in ["MAVEN_HOME", "M2_HOME"] {
                if user.iter().any(|(k, v)| k.eq_ignore_ascii_case(name) && path_eq(v, &target_s)) {
                    env_changed |= user_env_set_opt(name, None).is_ok();
                }
            }
        }
    }
    if env_changed {
        broadcast_env_change();
    }
    Ok(())
}

#[tauri::command]
pub async fn uninstall_runtime(kind: String, path: String) -> Result<(), String> {
    let kind = RuntimeKind::from_str(&kind)?;
    tokio::task::spawn_blocking(move || uninstall_runtime_sync(kind, path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(entries: &[&str]) -> HashSet<String> {
        entries.iter().map(|e| path_key(e)).collect()
    }

    #[test]
    fn strip_known_removes_runtimes_and_keeps_order() {
        let machine = r"C:\Windows\system32;C:\Program Files\nodejs\;C:\Windows;D:\jdk-17\bin;C:\Tools";
        let k = known(&[r"c:\program files\nodejs", r"D:\JDK-17\bin\"]);
        assert_eq!(strip_known(machine, &k), r"C:\Windows\system32;C:\Windows;C:\Tools");
    }

    #[test]
    fn strip_known_without_matches_is_identity() {
        let machine = r"C:\Windows\system32;C:\Windows";
        assert_eq!(strip_known(machine, &known(&[r"D:\node"])), machine);
    }

    #[test]
    fn rewrite_path_moves_target_to_front_once() {
        let user = r"C:\a;D:\node18;C:\b;D:\node20\";
        let k = known(&[r"D:\node18", r"D:\node20"]);
        assert_eq!(rewrite_path(user, &k, &[r"D:\node20".to_string()]), r"D:\node20;C:\a;C:\b");
    }

    #[test]
    fn machine_env_script_sets_and_deletes_in_one_run() {
        let script = machine_env_script(&[("Path", Some(r"C:\it's")), ("JAVA_HOME", None)]);
        assert!(script.contains(r"Set-ItemProperty -Path $k -Name 'Path' -Value 'C:\it''s' -Type ExpandString"));
        assert!(script.contains("Remove-ItemProperty -Path $k -Name 'JAVA_HOME'"));
        assert!(!script.contains("SetEnvironmentVariable"));
    }

    #[test]
    fn jdk_known_bins_include_oracle_javapath_shim() {
        let snap = EnvSnapshot {
            machine_path: r"C:\ProgramData\Oracle\Java\javapath;C:\WINDOWS\system32".into(),
            user_path: r"C:\Program Files (x86)\Common Files\Oracle\Java\javapath\;D:\jdk21\bin".into(),
            user_java_home: String::new(),
            machine_java_home: String::new(),
        };
        let known = known_runtime_bins(&RuntimeKind::Jdk, &[], &snap);
        assert!(known.contains(&path_key(r"C:\ProgramData\Oracle\Java\javapath")));
        assert!(known.contains(&path_key(r"C:\Program Files (x86)\Common Files\Oracle\Java\javapath")));
        assert!(!known.contains(&path_key(r"C:\WINDOWS\system32")));
        assert!(known_runtime_bins(&RuntimeKind::Node, &[], &snap).is_empty());
        assert_eq!(strip_known(&snap.machine_path, &known), r"C:\WINDOWS\system32");
    }

    #[test]
    fn jdk_release_names_map_to_installed_versions() {
        assert_eq!(jdk_installed_aliases("jdk-21.0.2+13"), vec!["21.0.2", "21.0.2+13"]);
        assert_eq!(jdk_installed_aliases("jdk8u504-b01"), vec!["1.8.0_504"]);
        let urls = archive_urls(&RuntimeKind::Jdk, "jdk8u504-b01").unwrap();
        assert_eq!(urls.len(), 1, "JDK 8 has no TUNA mirror URL: {urls:?}");
    }

    #[test]
    fn maven_metadata_keeps_ga_3_plus_newest_first() {
        let xml = "<versions><version>2.2.1</version><version>3.6.3</version><version>3.9.11</version>\
                   <version>4.0.0-rc-4</version><version>3.9.9</version><version>3.0-alpha-1</version></versions>";
        assert_eq!(parse_maven_versions(xml), vec!["3.9.11", "3.9.9", "3.6.3"]);
    }

    #[test]
    fn maven_home_version_comes_from_core_jar() {
        let home = std::env::temp_dir().join(format!("aiwb-mvn-test-{}", std::process::id()));
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::create_dir_all(home.join("lib")).unwrap();
        fs::write(home.join("bin").join("mvn.cmd"), "").unwrap();
        fs::write(home.join("lib").join("maven-core-3.9.6.jar"), "").unwrap();
        assert!(is_maven_home(&home));
        assert_eq!(read_maven_version(&home).as_deref(), Some("3.9.6"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn custom_runtimes_file_without_maven_key_still_parses() {
        let parsed: CustomRuntimesFile = serde_json::from_str(r#"{"node":["D:\\n"],"jdk":[]}"#).unwrap();
        assert_eq!(parsed.node, vec![r"D:\n"]);
        assert!(parsed.maven.is_empty());
    }

    #[test]
    fn protected_dirs_block_roots_and_ancestors_only() {
        let protected = vec![path_key(r"C:\Users\me"), path_key(r"C:\Program Files")];
        assert!(is_protected_dir(&path_key(r"D:\"), &protected));
        assert!(is_protected_dir(&path_key(r"C:\Users"), &protected));
        assert!(is_protected_dir(&path_key(r"C:\Program Files\"), &protected));
        assert!(!is_protected_dir(&path_key(r"C:\Program Files\Java\jdk-17"), &protected));
        assert!(!is_protected_dir(&path_key(r"C:\Users\me\.jvman\jdk\temurin\jdk-21"), &protected));
        assert!(!is_protected_dir(&path_key(r"D:\nodejs"), &protected));
    }

    #[test]
    fn shared_bin_dir_with_node_exe_is_not_a_runtime_home() {
        let dir = std::env::temp_dir().join(format!("aiwb-shim-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("node.exe"), "").unwrap();
        assert!(!looks_like_runtime_home(&RuntimeKind::Node, &dir));
        fs::create_dir_all(dir.join("node_modules").join("npm")).unwrap();
        assert!(looks_like_runtime_home(&RuntimeKind::Node, &dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_switch_puts_home_and_scripts_first() {
        let kind = RuntimeKind::Python;
        let k: HashSet<String> = [r"D:\py38", r"D:\py312"]
            .iter()
            .flat_map(|h| kind.path_entries(h))
            .map(|p| path_key(&p))
            .collect();
        let user = r"D:\py38\Scripts;C:\tools;D:\py38;C:\Users\me\AppData\Local\Microsoft\WindowsApps";
        assert_eq!(
            rewrite_path(user, &k, &kind.path_entries(r"D:\py312")),
            r"D:\py312;D:\py312\Scripts;C:\tools;C:\Users\me\AppData\Local\Microsoft\WindowsApps"
        );
    }

    #[test]
    fn python_versions_parse_banner_and_nuget_index() {
        assert_eq!(parse_python_version("Python 3.12.10\r\n").as_deref(), Some("3.12.10"));
        assert_eq!(parse_python_version("Python 2.7.18").as_deref(), Some("2.7.18"));
        assert_eq!(parse_python_version("'python' 不是内部或外部命令"), None);
        let index = serde_json::json!({"versions": ["3.7.9", "3.8.10", "3.13.0-rc1", "3.12.10", "3.12.9", "3.15.0"]});
        assert_eq!(parse_python_versions(&index), vec!["3.15.0", "3.12.10", "3.12.9", "3.8.10"]);
    }

    #[test]
    fn python_home_needs_stdlib_so_store_stub_and_venv_are_skipped() {
        let dir = std::env::temp_dir().join(format!("aiwb-py-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("python.exe"), "").unwrap();
        assert!(!is_python_home(&dir));
        fs::create_dir_all(dir.join("Lib")).unwrap();
        fs::write(dir.join("Lib").join("os.py"), "").unwrap();
        assert!(is_python_home(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_java_version_reads_quoted_version() {
        let out = "openjdk version \"17.0.9\" 2023-10-17\nOpenJDK Runtime Environment";
        assert_eq!(parse_java_version(out).as_deref(), Some("17.0.9"));
    }
}
