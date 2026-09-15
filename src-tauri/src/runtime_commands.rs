//! Node.js / JDK version detection and env switching (Windows).
//!
//! Rewrites User (and when needed Machine) PATH / JAVA_HOME so new shells
//! pick up the selected runtime. Does not download versions.

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
}

impl RuntimeKind {
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "node" => Ok(RuntimeKind::Node),
            "jdk" | "java" => Ok(RuntimeKind::Jdk),
            other => Err(format!("Unknown runtime kind: {}", other)),
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CustomRuntimesFile {
    node: Vec<String>,
    jdk: Vec<String>,
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

fn read_node_version(bin_dir: &Path) -> Option<String> {
    let exe = bin_dir.join("node.exe");
    if !exe.is_file() {
        let exe_unix = bin_dir.join("node");
        if exe_unix.is_file() {
            return run_capture(&exe_unix, &["-v"]).map(|v| v.trim_start_matches('v').to_string());
        }
        return None;
    }
    run_capture(&exe, &["-v"]).map(|v| v.trim_start_matches('v').to_string())
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
        if let Some(out) = run_capture(&java, &["-version"]) {
            for line in out.lines() {
                if let Some(idx) = line.find('"') {
                    let rest = &line[idx + 1..];
                    if let Some(end) = rest.find('"') {
                        return Some(rest[..end].to_string());
                    }
                }
            }
        }
    }
    None
}

fn is_jdk_home(dir: &Path) -> bool {
    dir.join("bin").join("java.exe").is_file()
}

fn is_node_bin(dir: &Path) -> bool {
    dir.join("node.exe").is_file()
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

fn broadcast_env_change() {
    #[cfg(target_os = "windows")]
    {
        let script = r#"
Add-Type -Namespace Win32 -Name Native -MemberDefinition @'
[DllImport("user32.dll", SetLastError=true, CharSet=CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
$HWND_BROADCAST = [IntPtr]0xffff
$result = [UIntPtr]::Zero
[void][Win32.Native]::SendMessageTimeout($HWND_BROADCAST, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
"#;
        let _ = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output();
    }
}

fn env_set(name: &str, value: &str, target: &str, elevate: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let name_esc = name.replace('\'', "''");
        let value_esc = value.replace('\'', "''");
        let target_esc = target.replace('\'', "''");
        let inner = format!(
            "[Environment]::SetEnvironmentVariable('{name_esc}','{value_esc}','{target_esc}'); if (-not $?) {{ exit 1 }}"
        );

        let output = if elevate {
            let script_path = std::env::temp_dir().join(format!(
                "ai_workbench_set_env_{}.ps1",
                std::process::id()
            ));
            fs::write(&script_path, format!("{}\nexit 0\n", inner))
                .map_err(|e| format!("Failed to write elevate script: {}", e))?;
            let script_arg = script_path.to_string_lossy().replace('\'', "''");
            let launcher = format!(
                "$p = Start-Process -FilePath powershell -Verb RunAs -Wait -PassThru -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','{script_arg}'); if ($null -eq $p) {{ exit 1223 }}; exit $p.ExitCode"
            );
            let out = hidden_cmd("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
                .output()
                .map_err(|e| format!("Failed to elevate env set: {}", e));
            let _ = fs::remove_file(&script_path);
            out?
        } else {
            hidden_cmd("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &inner])
                .output()
                .map_err(|e| format!("Failed to set {} env {}: {}", target, name, e))?
        };

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let code = output.status.code().unwrap_or(-1);
            if elevate
                && (code == 1223
                    || err.to_ascii_lowercase().contains("canceled")
                    || err.to_ascii_lowercase().contains("cancelled"))
            {
                return Err("已取消管理员授权，无法修改系统 PATH".to_string());
            }
            return Err(format!(
                "Failed to set {} env {} (exit {}): {}",
                target, name, code, err
            ));
        }
        // Broadcast is deliberately left to the caller: a switch writes several
        // variables and only needs to notify the system once, at the end.
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (name, value, target, elevate);
        Err("Runtime switching is only supported on Windows".to_string())
    }
}

fn user_env_set(name: &str, value: &str) -> Result<(), String> {
    env_set(name, value, "User", false)
}

fn machine_env_set_elevated(name: &str, value: &str) -> Result<(), String> {
    env_set(name, value, "Machine", true)
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

fn rewrite_path(existing: &str, known: &HashSet<String>, insert_bin: &str) -> String {
    let mut parts: Vec<String> = split_path(existing)
        .into_iter()
        .filter(|p| !known.contains(&path_key(p)))
        .collect();
    parts.retain(|p| !path_eq(p, insert_bin));
    parts.insert(0, insert_bin.to_string());
    join_path(&parts)
}

fn machine_bins_set(snap: &EnvSnapshot) -> HashSet<String> {
    split_path(&snap.machine_path)
        .into_iter()
        .map(|p| path_key(&p))
        .collect()
}

fn collect_known_node_bins(list: &[RuntimeVersion]) -> HashSet<String> {
    list.iter()
        .filter(|r| r.kind == "node")
        .map(|r| path_key(&r.bin_path))
        .collect()
}

fn collect_known_jdk_bins(list: &[RuntimeVersion]) -> HashSet<String> {
    list.iter()
        .filter(|r| r.kind == "jdk")
        .map(|r| path_key(&r.bin_path))
        .collect()
}

fn mark_active(list: &mut [RuntimeVersion], kind: &RuntimeKind, snap: &EnvSnapshot) {
    let machine_set = machine_bins_set(snap);
    for item in list.iter_mut() {
        item.on_machine_path = machine_set.contains(&path_key(&item.bin_path));
    }

    match kind {
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
    let known = match kind {
        RuntimeKind::Node => collect_known_node_bins(list),
        RuntimeKind::Jdk => collect_known_jdk_bins(list),
    };

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
            Some("系统 PATH 中已有其他运行时条目，需管理员权限才能真正切换".to_string())
        } else {
            None
        },
    }
}

fn verify_resolved(kind: &RuntimeKind, expected_bin: &str) -> (bool, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        let exe = match kind {
            RuntimeKind::Node => "node",
            RuntimeKind::Jdk => "java",
        };
        let ver_args = match kind {
            RuntimeKind::Node => "-v",
            RuntimeKind::Jdk => "-version",
        };
        let loc_script = format!(
            "$m=[Environment]::GetEnvironmentVariable('Path','Machine');$u=[Environment]::GetEnvironmentVariable('Path','User');$env:Path=($m+';'+$u); $p=(Get-Command {exe} -ErrorAction SilentlyContinue).Source; if(-not $p){{ '' }} else {{ $p }}"
        );
        let loc = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &loc_script])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        let verified_path = if !loc.is_empty() {
            Path::new(&loc)
                .parent()
                .map(|p| normalize_path(p))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let path_ok = !verified_path.is_empty() && path_eq(&verified_path, expected_bin);

        let ver_script = format!(
            "$m=[Environment]::GetEnvironmentVariable('Path','Machine');$u=[Environment]::GetEnvironmentVariable('Path','User');$env:Path=($m+';'+$u); & {exe} {ver_args} 2>&1 | Out-String"
        );
        let ver_out = hidden_cmd("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ver_script])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        let version = match kind {
            RuntimeKind::Node => {
                let v = ver_out
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_start_matches('v');
                if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                }
            }
            RuntimeKind::Jdk => ver_out.lines().find_map(|line| {
                let line = line.trim();
                line.find('"').and_then(|idx| {
                    let rest = &line[idx + 1..];
                    rest.find('"').map(|end| rest[..end].to_string())
                })
            }),
        };

        (path_ok, version)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (kind, expected_bin);
        (false, None)
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
    match kind {
        RuntimeKind::Node => {
            customs.node.retain(|x| !path_eq(x, &path));
        }
        RuntimeKind::Jdk => {
            customs.jdk.retain(|x| !path_eq(x, &path));
        }
    }
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

    if let Some(bin) = bin_path.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) {
        let bin = PathBuf::from(normalize_path(Path::new(&bin)));
        if bin.is_dir() {
            let current = if path_value.trim().is_empty() {
                bin.to_string_lossy().to_string()
            } else {
                format!("{};{}", bin.to_string_lossy(), path_value)
            };
            path_value = current;
            if matches!(kind, RuntimeKind::Jdk) {
                if let Some(home) = bin.parent() {
                    java_home = home.to_string_lossy().to_string();
                }
            }
        }
    }

    let probe = match kind {
        RuntimeKind::Node => "node -v",
        RuntimeKind::Jdk => "java -version",
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
    let p = PathBuf::from(path.trim());
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

    let known = match kind {
        RuntimeKind::Node => collect_known_node_bins(&list),
        RuntimeKind::Jdk => collect_known_jdk_bins(&list),
    };

    let plan = compute_switch_plan(&kind, &target, &list, &snap);
    let mut elevated = false;
    let mut machine_error: Option<String> = None;

    // Machine scope first: it is the only step that can be aborted (UAC prompt).
    // Writing the User scope first left a half-switched environment whenever the
    // prompt was cancelled — User PATH already rewritten, Machine untouched — and
    // the caller could only report that as a plain failure.
    if plan.needs_elevation {
        let new_machine = rewrite_path(&snap.machine_path, &known, &target.bin_path);
        match machine_env_set_elevated("Path", &new_machine) {
            Ok(()) => elevated = true,
            Err(e) => machine_error = Some(e),
        }
    }
    let machine_java_home_stale = matches!(kind, RuntimeKind::Jdk)
        && !snap.machine_java_home.is_empty()
        && !path_eq(&snap.machine_java_home, &target.path);
    if machine_java_home_stale && machine_error.is_none() {
        match machine_env_set_elevated("JAVA_HOME", &target.path) {
            Ok(()) => elevated = true,
            Err(e) => machine_error = Some(e),
        }
    }

    if let Some(err) = machine_error {
        // The User scope is deliberately untouched, so the environment stays
        // consistent. Report which scope did change instead of a bare failure.
        if elevated {
            // The machine scope did change, so other processes still need the nudge.
            broadcast_env_change();
        }
        let verified_version = if elevated {
            verify_resolved(&kind, &target.bin_path).1
        } else {
            None
        };
        let message = if elevated {
            format!(
                "系统 PATH 已更新，但系统 JAVA_HOME 未更新：{}；用户环境变量未改动",
                err
            )
        } else {
            format!("未修改任何环境变量：{}", err)
        };
        return Ok(SwitchResult {
            message,
            version: target.version,
            verified: false,
            verified_version,
            elevated,
        });
    }

    // User scope last: these writes cannot prompt, and the machine scope is
    // already committed, so a failure here must not read as a no-op.
    let mut user_error: Option<String> = None;
    let new_user = rewrite_path(&snap.user_path, &known, &target.bin_path);
    if let Err(e) = user_env_set("Path", &new_user) {
        user_error = Some(e);
    } else if matches!(kind, RuntimeKind::Jdk) {
        if let Err(e) = user_env_set("JAVA_HOME", &target.path) {
            user_error = Some(e);
        }
    }

    if user_error.is_none() {
        // One broadcast for the whole switch — `env_set` used to notify on every write.
        broadcast_env_change();
    }

    let (mut verified, verified_version) = verify_resolved(&kind, &target.bin_path);
    if user_error.is_some() {
        verified = false;
    }

    let label = match kind {
        RuntimeKind::Node => format!("Node.js v{}", target.version),
        RuntimeKind::Jdk => format!("JDK {}", target.version),
    };
    let mut message = if let Some(err) = user_error.as_deref() {
        if elevated {
            format!("系统环境变量已更新，但用户环境变量写入失败：{}。请重试一次", err)
        } else {
            format!("用户环境变量写入失败：{}", err)
        }
    } else if verified {
        format!("已切换至 {}（新开终端生效）", label)
    } else {
        format!(
            "已写入环境变量（{}），但校验未通过，请确认 PATH 或重启终端",
            label
        )
    };
    if elevated && user_error.is_none() {
        message.push_str("；已用管理员权限更新系统 PATH");
    }
    if verified {
        if let Some(ref v) = verified_version {
            message.push_str(&format!("；校验版本 {}", v));
        }
    }

    Ok(SwitchResult {
        message,
        version: target.version,
        verified,
        verified_version,
        elevated,
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
const ADOPTIUM_RELEASES_URL: &str = "https://api.adoptium.net/v3/info/release_names?architecture=x64&image_type=jdk&os=windows&page=0&page_size=80&project=jdk&release_type=ga&sort_order=DESC&vendor=eclipse";

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
            let major = semver.split('.').next().unwrap_or_default().to_string();
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
    })
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
            let percent = if total > 0 {
                ((downloaded * 100) / total).min(99) as u8
            } else {
                0
            };
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
    Ok(managed_runtimes_dir()?.join(match kind {
        RuntimeKind::Node => "node",
        RuntimeKind::Jdk => "jdk",
    }))
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

async fn list_installable_jdk(installed: &[RuntimeVersion]) -> Result<Vec<InstallableVersion>, String> {
    let data = fetch_json(ADOPTIUM_RELEASES_URL).await?;
    let names = data
        .get("releases")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "版本列表格式异常".to_string())?;

    let mut seen_major: HashSet<u64> = HashSet::new();
    let mut out = Vec::new();
    for name in names {
        let name = name.as_str().unwrap_or_default().to_string();
        let Some(rest) = name.strip_prefix("jdk-") else {
            continue;
        };
        let Some(major) = rest.split('.').next().and_then(|m| m.parse::<u64>().ok()) else {
            continue;
        };
        if !seen_major.insert(major) {
            continue;
        }
        // `java -version` reports the semver without the build number.
        let semver = rest.split('+').next().unwrap_or(rest).to_string();
        out.push(InstallableVersion {
            installed: installed
                .iter()
                .any(|i| i.version == semver || i.version == rest),
            version: name,
            lts: matches!(major, 8 | 11 | 17 | 21 | 25),
        });
        if out.len() >= 20 {
            break;
        }
    }
    if out.is_empty() {
        return Err("JDK 发布列表为空".to_string());
    }
    Ok(out)
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
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let _ = fs::remove_dir_all(&temp);
            fs::create_dir_all(&temp).map_err(|e| format!("创建临时目录失败: {}", e))?;
            extract_zip(&archive, &temp)?;
            promote_single_dir(&temp, &dest)?;
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

/// Only versions installed by this app (inside the managed root) can be removed.
fn uninstall_runtime_sync(kind: RuntimeKind, path: String) -> Result<(), String> {
    let root = managed_kind_dir(&kind)?;
    let root_key = path_key(&normalize_path(&root));
    let target = PathBuf::from(normalize_path(Path::new(path.trim())));
    let target_key = path_key(&target.to_string_lossy());
    if target_key == root_key || !target_key.starts_with(&root_key) {
        return Err("只能卸载由本应用安装的版本".to_string());
    }
    if !target.is_dir() {
        return Err("安装目录不存在".to_string());
    }
    let list = build_list(&kind)?;
    if list
        .iter()
        .any(|item| item.active && path_eq(&item.path, &target.to_string_lossy()))
    {
        return Err("该版本正在使用中，请先切换到其它版本".to_string());
    }
    remove_dir_with_retry(&target, 5)
}

#[tauri::command]
pub async fn uninstall_runtime(kind: String, path: String) -> Result<(), String> {
    let kind = RuntimeKind::from_str(&kind)?;
    tokio::task::spawn_blocking(move || uninstall_runtime_sync(kind, path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}
