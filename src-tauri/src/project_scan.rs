//! Recursive discovery of testable projects under a directory.
//!
//! Pure (filesystem only, no tauri) so it can be run and measured from a scratch crate
//! against the user's real trees.
//!
//! The rule that differs from the git scanner: a Maven *aggregator* pom (one with
//! `<modules>`) is a container, not a project — we must keep descending through it,
//! whereas `.git` stops the descent. Conversely a `package.json`/`go.mod` leaf stops it,
//! because descending into `node_modules`-shaped trees costs time and yields nothing.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::maven_pom::{self, path_text, PomInfo};

/// What `detect_project_type` answers for one directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetectionResult {
    pub path: String,
    pub detected: bool,
    pub project_type: Option<String>,
    pub framework: Option<String>,
    pub test_command: Option<String>,
    /// Evidence token (`pom.xml`, `package.json + test script`, `none`, …), never prose.
    pub reason: String,
}

/// One project found by the recursive scan, ready to be added in bulk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedProject {
    pub path: String,
    pub name: String,
    pub project_type: String,
    pub framework: String,
    pub test_command: String,
    pub reason: String,
    /// Where the command must run: a reactor root for a Maven module, otherwise its own
    /// directory (then empty, meaning "the project path").
    pub working_dir: String,
    pub reactor_root: String,
    pub reactor_module: String,
    pub artifact_id: String,
    /// Evidence tokens: `wrapper-missing`, `no-test-sources`, `reactor`.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOutcome {
    pub projects: Vec<ScannedProject>,
    pub dirs_walked: usize,
    pub skipped_junk: usize,
}

/// Directories that never contain a project root worth registering.
pub const JUNK_DIRS: &[&str] = &[
    ".git", ".idea", ".vscode", ".vs", ".gradle", ".mvn", ".svn", "node_modules", "target", "build",
    "dist", "out", "bin", "obj", "logs", "coverage", "__pycache__", ".pytest_cache", ".mypy_cache",
    ".venv", "venv", ".next", ".nuxt", ".turbo", "generated-sources", "graphene",
];

pub fn is_junk(name: &str) -> bool {
    let lower = name.to_lowercase();
    JUNK_DIRS.contains(&lower.as_str()) || lower.starts_with('.')
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Detect a non-Maven project from its markers. Maven is handled by the caller because
/// it needs the pom's content (aggregator vs leaf, wrapper health).
pub fn detect_at(dir: &Path) -> Option<ProjectDetectionResult> {
    let path = path_text(dir);
    if let Some(js) = node_project(dir) {
        return Some(ProjectDetectionResult { path, detected: true, ..js });
    }
    if dir.join("Cargo.toml").is_file() {
        return Some(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("rust".to_string()),
            framework: Some("cargo".to_string()),
            test_command: Some("cargo test".to_string()),
            reason: "Cargo.toml".to_string(),
        });
    }
    if ["requirements.txt", "pytest.ini", "pyproject.toml", "setup.py"]
        .iter()
        .any(|marker| dir.join(marker).is_file())
    {
        return Some(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("python".to_string()),
            framework: Some("pytest".to_string()),
            test_command: Some("pytest".to_string()),
            reason: "pyproject.toml / setup.py / requirements.txt".to_string(),
        });
    }
    if dir.join("go.mod").is_file() {
        return Some(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("go".to_string()),
            framework: Some("gotest".to_string()),
            test_command: Some("go test ./...".to_string()),
            reason: "go.mod".to_string(),
        });
    }
    None
}

/// `package.json` with a `test` script. Returns `None` (so the caller can still find a
/// pom.xml) when the script is missing — a Java service with a frontend folder is common.
fn node_project(dir: &Path) -> Option<ProjectDetectionResult> {
    let content = fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let scripts = json.get("scripts")?.as_object()?;
    if !scripts.contains_key("test") {
        return None;
    }
    let deps = |section: &str, key: &str| {
        json.get(section).and_then(|v| v.get(key)).is_some()
    };
    let framework = if deps("devDependencies", "vitest") || deps("dependencies", "vitest") {
        "vitest"
    } else if deps("devDependencies", "@playwright/test") {
        "playwright"
    } else if deps("devDependencies", "mocha") {
        "mocha"
    } else {
        "jest"
    };
    Some(ProjectDetectionResult {
        path: String::new(),
        detected: true,
        project_type: Some("frontend".to_string()),
        framework: Some(framework.to_string()),
        test_command: Some("npm test".to_string()),
        reason: "package.json + test script".to_string(),
    })
}

/// A Cargo workspace or an npm workspaces root contains projects but is not one itself.
fn is_container(dir: &Path) -> bool {
    if let Ok(content) = fs::read_to_string(dir.join("Cargo.toml")) {
        if content.contains("[workspace]") {
            return true;
        }
    }
    if let Ok(content) = fs::read_to_string(dir.join("package.json")) {
        if content.contains("\"workspaces\"") {
            return true;
        }
    }
    false
}

/// Scan `base` and up to `max_depth` levels below it. `base` itself is considered too,
/// so pointing the picker straight at a module still works.
pub fn scan_projects(base: &Path, max_depth: u32) -> ScanOutcome {
    let mut outcome = ScanOutcome::default();
    let mut seen: Vec<String> = Vec::new();
    let mut frontier: Vec<(PathBuf, u32)> = vec![(base.to_path_buf(), 0)];

    while let Some((dir, depth)) = frontier.pop() {
        if depth > max_depth {
            continue;
        }
        outcome.dirs_walked += 1;
        if let Some(project) = project_at(&dir) {
            let key = project.path.clone();
            if !seen.contains(&key) {
                seen.push(key);
                outcome.projects.push(project);
            }
            // A detected leaf does not stop the walk for maven (nested modules) but does
            // for the single-root ecosystems, which is decided inside `project_at`.
        }
        if dir.join("pom.xml").is_file() || is_container(&dir) || !is_project_root(&dir) {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if is_junk(&name) {
                    outcome.skipped_junk += 1;
                    continue;
                }
                frontier.push((path, depth + 1));
            }
        }
    }

    outcome.projects.sort_by(|a, b| a.path.cmp(&b.path));
    outcome
}

/// Does this directory advertise itself as a project root by any marker?
fn is_project_root(dir: &Path) -> bool {
    ["pom.xml", "package.json", "Cargo.toml", "go.mod", "pyproject.toml", "setup.py", "requirements.txt", "pytest.ini"]
        .iter()
        .any(|marker| dir.join(marker).is_file())
}

/// Build the registration row for one directory, or `None` when it is a container
/// (maven aggregator) or has no recognised test entry point.
pub fn project_at(dir: &Path) -> Option<ScannedProject> {
    let path = path_text(dir);
    let name = dir_name(dir);

    if dir.join("pom.xml").is_file() {
        let pom = maven_pom::read_pom(dir)?;
        if !pom.is_leaf_project() {
            // A reactor root or a `<packaging>pom</packaging>` parent is where builds run,
            // not a test target of its own — keep descending to its modules.
            return None;
        }
        return Some(maven_project(dir, &path, &name, &pom));
    }

    let detected = detect_at(dir)?;
    if !detected.detected {
        return None;
    }
    Some(ScannedProject {
        path,
        name,
        project_type: detected.project_type.unwrap_or_else(|| "custom".to_string()),
        framework: detected.framework.unwrap_or_else(|| "custom".to_string()),
        test_command: detected.test_command.unwrap_or_default(),
        reason: detected.reason,
        working_dir: String::new(),
        reactor_root: String::new(),
        reactor_module: String::new(),
        artifact_id: String::new(),
        notes: Vec::new(),
    })
}

fn maven_project(dir: &Path, path: &str, name: &str, pom: &PomInfo) -> ScannedProject {
    let launcher = maven_pom::maven_launcher(dir);
    let reactor = maven_pom::find_reactor_root(dir, parent_of(dir).as_deref().unwrap_or(dir));
    let mut notes = Vec::new();
    if maven_pom::is_wrapper_broken(dir) {
        notes.push("wrapper-missing".to_string());
    }
    if !pom.has_test_sources {
        notes.push("no-test-sources".to_string());
    }
    if pom.skip_tests {
        notes.push("skip-tests-property".to_string());
    }

    // One source of truth for the argument shape: `plan_maven_run` also backs the
    // run-time path, so a scanned command and a re-planned one cannot drift.
    let plan = maven_pom::plan_maven_run(dir, reactor.as_ref(), &pom.coord.artifact_id, None);
    let test_command = format!("{} {}", launcher, plan.args.join(" "));
    let mut working_dir = String::new();
    let (reactor_root, reactor_module) = match &reactor {
        Some(info) => {
            notes.push("reactor".to_string());
            working_dir = relative_to_parent(dir, &info.root);
            (info.root.clone(), info.relative_module.clone())
        }
        None => (String::new(), String::new()),
    };

    ScannedProject {
        path: path.to_string(),
        name: name.to_string(),
        project_type: "backend".to_string(),
        framework: "maven".to_string(),
        test_command,
        reason: "pom.xml".to_string(),
        working_dir,
        reactor_root,
        reactor_module,
        artifact_id: pom.coord.artifact_id.clone(),
        notes,
    }
}

fn parent_of(dir: &Path) -> Option<PathBuf> {
    dir.parent().map(|p| p.to_path_buf())
}

/// The project-relative directory the build should run in (`..`, `../..`), which is how
/// a reactor module's root is carried without a new column.
fn relative_to_parent(module: &Path, root: &str) -> String {
    let root_path = PathBuf::from(root.replace('/', std::path::MAIN_SEPARATOR_STR));
    let Ok(rest) = module.strip_prefix(&root_path) else {
        return String::new();
    };
    let up = rest.components().count();
    if up == 0 {
        return String::new();
    }
    vec![".."; up].join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiwb-scan-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_package_json_without_a_test_script_does_not_hide_the_pom() {
        let dir = scratch("both");
        touch(&dir, "package.json", r#"{"scripts":{"build":"vite build"}}"#);
        touch(&dir, "pom.xml", "<project><artifactId>svc</artifactId></project>");
        let project = project_at(&dir).expect("the java marker still counts");
        assert_eq!(project.framework, "maven");
        assert_eq!(project.test_command, "mvn -B test -DskipTests=false");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_rust_python_go_are_recognised() {
        let dir = scratch("markers");
        touch(&dir.join("web"), "package.json", r#"{"scripts":{"test":"vitest run"},"devDependencies":{"vitest":"^2.0.0"}}"#);
        touch(&dir.join("api"), "go.mod", "module x\n");
        touch(&dir.join("ml"), "pyproject.toml", "[project]\nname='x'\n");
        touch(&dir.join("svc"), "Cargo.toml", "[package]\nname='x'\n");
        let outcome = scan_projects(&dir, 2);
        // Sorted by path: api < ml < svc < web.
        let frameworks: Vec<&str> = outcome.projects.iter().map(|p| p.framework.as_str()).collect();
        assert_eq!(frameworks, vec!["gotest", "pytest", "cargo", "vitest"], "got {:?}", outcome.projects.iter().map(|p| &p.path).collect::<Vec<_>>());
        let commands: Vec<&str> = outcome.projects.iter().map(|p| p.test_command.as_str()).collect();
        assert_eq!(commands, vec!["go test ./...", "pytest", "cargo test", "npm test"]);
        assert!(outcome.projects.iter().all(|p| p.reason != "none"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_aggregator_is_a_container_while_its_modules_are_projects() {
        let dir = scratch("reactor");
        let agg = dir.join("svc");
        touch(&agg, "pom.xml", "<project><artifactId>svc</artifactId><packaging>pom</packaging><modules><module>svc-server</module><module>svc-sdk</module></modules></project>");
        for module in ["svc-server", "svc-sdk"] {
            touch(&agg.join(module), "pom.xml", &format!("<project><parent><groupId>g</groupId><artifactId>svc</artifactId><version>1</version></parent><artifactId>{}</artifactId></project>", module));
            fs::create_dir_all(agg.join(module).join("src/test/java/x")).unwrap();
            fs::write(agg.join(module).join("src/test/java/x/FooTest.java"), "class FooTest {}").unwrap();
        }
        let outcome = scan_projects(&dir, 3);
        assert_eq!(outcome.projects.len(), 2, "the aggregator itself is not offered");
        for project in &outcome.projects {
            assert_eq!(project.reactor_root, path_text(&agg), "both hang off the same root");
            assert_eq!(project.working_dir, "..", "the build runs one level up");
            assert!(project.test_command.starts_with("mvn -B -pl :"), "{}", project.test_command);
            assert!(project.test_command.contains("-am test -DskipTests=false"));
            assert!(project.notes.contains(&"reactor".to_string()));
            assert!(project.notes.contains(&"no-test-sources".to_string()) == false);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_flat_parent_without_modules_is_not_mistaken_for_a_reactor() {
        let dir = scratch("flat");
        let parent = dir.join("p1");
        touch(&parent, "pom.xml", "<project><artifactId>parent.pom</artifactId><packaging>pom</packaging></project>");
        let module = parent.join("aio.ui");
        touch(&module, "pom.xml", "<project><artifactId>aio.ui</artifactId></project>");
        let outcome = scan_projects(&dir, 3);
        let names: Vec<&str> = outcome.projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"aio.ui"), "got {:?}", names);
        let module = outcome.projects.iter().find(|p| p.name == "aio.ui").unwrap();
        assert!(module.reactor_root.is_empty(), "no <modules> means no reactor: {}", module.reactor_root);
        assert_eq!(module.test_command, "mvn -B test -DskipTests=false");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn junk_directories_are_skipped_and_counted() {
        let dir = scratch("junk");
        touch(&dir.join("real"), "pom.xml", "<project><artifactId>real</artifactId></project>");
        touch(&dir.join("node_modules").join("pkg"), "package.json", r#"{"scripts":{"test":"jest"}}"#);
        touch(&dir.join("svc").join("target"), "pom.xml", "<project><artifactId>ghost</artifactId></project>");
        let outcome = scan_projects(&dir, 4);
        assert_eq!(outcome.projects.len(), 1, "got {:?}", outcome.projects.iter().map(|p| &p.path).collect::<Vec<_>>());
        assert!(outcome.skipped_junk >= 2, "skipped {}", outcome.skipped_junk);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn depth_cap_bounds_the_walk() {
        let dir = scratch("depth");
        let mut deep = dir.clone();
        for level in 0..4 {
            deep = deep.join(format!("l{}", level));
            touch(&deep, "pom.xml", &format!("<project><artifactId>m{}</artifactId></project>", level));
        }
        let shallow = scan_projects(&dir, 1);
        let deeper = scan_projects(&dir, 4);
        assert_eq!(shallow.projects.len(), 1);
        assert_eq!(deeper.projects.len(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_base_directory_itself_is_offered() {
        let dir = scratch("base");
        touch(&dir, "pom.xml", "<project><artifactId>root-app</artifactId></project>");
        let outcome = scan_projects(&dir, 1);
        assert_eq!(outcome.projects.len(), 1);
        assert_eq!(outcome.projects[0].path, path_text(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
