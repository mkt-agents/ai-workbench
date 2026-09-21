//! A minimal Maven `pom.xml` reader and the single place where a maven test run's
//! command, working directory and report directory are decided.
//!
//! Pure (no tauri, no process spawning) so it compiles and runs from a scratch crate.
//! Deliberately not a full XML model: we read five scalars and one list, and every
//! real-world shape below was taken from the user's own poms — tabs, CRLF, commented-out
//! `<version>` inside `<parent>`, and `<parent>` appearing before the module's own
//! `<artifactId>`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::test_output_parsers::next_xml_tag;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PomCoord {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PomInfo {
    pub coord: PomCoord,
    /// `None` when the pom inherits it from `<parent>` (the common leaf-module case).
    pub parent: Option<PomCoord>,
    /// Defaults to `jar`, as Maven does.
    pub packaging: String,
    /// `<modules>` entries, empty for a leaf.
    pub modules: Vec<String>,
    /// `<properties><skipTests>` — the reason a "successful" run can execute nothing.
    pub skip_tests: bool,
    pub has_test_sources: bool,
    pub dir: String,
}

impl PomInfo {
    pub fn is_aggregator(&self) -> bool {
        !self.modules.is_empty()
    }

    pub fn is_leaf_project(&self) -> bool {
        !self.is_aggregator() && self.packaging != "pom"
    }
}

/// Read the handful of pom values we need. Returns `None` for anything we cannot make
/// sense of, so the caller degrades to "no reactor root" instead of failing the scan.
pub fn parse_pom_minimal(content: &str) -> Option<PomInfo> {
    let mut info = PomInfo {
        packaging: String::new(),
        ..Default::default()
    };
    // Stack of open element names, excluding the text-capturing scalar we may be in.
    let mut stack: Vec<String> = Vec::new();
    let mut in_parent = false;
    let mut in_modules = false;
    let mut in_properties = false;
    let mut pos = 0usize;

    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        if tag.close {
            match stack.pop() {
                Some(name) if name == tag.name => {}
                // Unbalanced input: stop rather than mis-attribute the rest.
                _ => return if info.coord.artifact_id.is_empty() { None } else { Some(finish(info)) },
            }
            if in_parent && stack.len() == 1 {
                in_parent = false;
            }
            if in_modules && stack.len() == 1 {
                in_modules = false;
            }
            if in_properties && stack.len() == 1 {
                in_properties = false;
            }
            continue;
        }

        let depth = stack.len();
        if !tag.empty {
            stack.push(tag.name.to_string());
        }

        // Only `<project>`'s direct children describe this pom.
        if depth != 1 {
            if in_modules && depth == 2 && tag.name == "module" && !tag.empty {
                let value = text_of(content, tag.end);
                if !value.is_empty() {
                    info.modules.push(value);
                }
            }
            if in_properties && depth == 2 && tag.name == "skipTests" {
                info.skip_tests = text_of(content, tag.end).trim() == "true";
            }
            if in_parent && depth == 2 {
                let value = text_of(content, tag.end);
                let coord = info.parent.get_or_insert_with(PomCoord::default);
                match tag.name {
                    "groupId" => coord.group_id = value,
                    "artifactId" if coord.artifact_id.is_empty() => coord.artifact_id = value,
                    "version" if coord.version.is_empty() => coord.version = value,
                    _ => {}
                }
            }
            continue;
        }

        let value = text_of(content, tag.end);
        match tag.name {
            "parent" => in_parent = true,
            "modules" => in_modules = true,
            "properties" => in_properties = true,
            "groupId" if info.coord.group_id.is_empty() => info.coord.group_id = value,
            "artifactId" if info.coord.artifact_id.is_empty() => info.coord.artifact_id = value,
            "version" if info.coord.version.is_empty() => info.coord.version = value,
            "packaging" if !value.is_empty() => info.packaging = value,
            _ => {}
        }
    }

    if info.coord.artifact_id.is_empty() {
        return None;
    }
    Some(finish(info))
}

fn finish(mut info: PomInfo) -> PomInfo {
    if info.packaging.is_empty() {
        info.packaging = "jar".to_string();
    }
    // A leaf almost always inherits groupId/version from its parent.
    if let Some(parent) = &info.parent {
        if info.coord.group_id.is_empty() {
            info.coord.group_id = parent.group_id.clone();
        }
        if info.coord.version.is_empty() {
            info.coord.version = parent.version.clone();
        }
    }
    info
}

/// The trimmed text between an open tag and the next `<`.
fn text_of(content: &str, from: usize) -> String {
    let tail = &content[from..];
    match tail.find('<') {
        Some(offset) => tail[..offset].trim().to_string(),
        None => tail.trim().to_string(),
    }
}

/// `/`-separated display text. Windows `canonicalize()` yields `\\?\D:\…` verbatim
/// paths, which must never reach the database or the UI.
pub fn path_text(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    match raw.strip_prefix("//?/") {
        Some(stripped) => stripped.to_string(),
        None => raw,
    }
}

/// Read `<dir>/pom.xml` and check for test sources.
pub fn read_pom(dir: &Path) -> Option<PomInfo> {
    let content = fs::read_to_string(dir.join("pom.xml")).ok()?;
    let mut info = parse_pom_minimal(&content)?;
    info.dir = path_text(dir);
    info.has_test_sources = has_test_sources(dir);
    Some(info)
}

/// Any `.java`/`.kt` under `src/test` — an empty `src/test/java` directory means the
/// module cannot contribute test cases even when the build is healthy.
pub fn has_test_sources(dir: &Path) -> bool {
    let root = dir.join("src").join("test");
    if !root.is_dir() {
        return false;
    }
    let mut frontier = vec![root];
    let mut visited = 0usize;
    while let Some(current) = frontier.pop() {
        visited += 1;
        if visited > 200 {
            return false;
        }
        let Ok(entries) = fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            if path.is_file() && (name.ends_with(".java") || name.ends_with(".kt")) {
                return true;
            }
            if path.is_dir() {
                frontier.push(path);
            }
        }
    }
    false
}

/// `mvnw.cmd` is committed without its `.mvn/wrapper` payload surprisingly often, and
/// then it dies trying to download the wrapper jar.
pub fn is_wrapper_broken(dir: &Path) -> bool {
    let wrapper = ["mvnw.cmd", "mvnw"]
        .iter()
        .any(|name| dir.join(name).is_file());
    if !wrapper {
        return false;
    }
    let jar = dir.join(".mvn").join("wrapper").join("maven-wrapper.jar");
    let props = dir.join(".mvn").join("wrapper").join("maven-wrapper.properties");
    !(jar.is_file() || props.is_file())
}

/// The program to launch maven with: the wrapper only when it is intact.
pub fn maven_launcher(dir: &Path) -> &'static str {
    if !is_wrapper_broken(dir) {
        if dir.join("mvnw.cmd").is_file() {
            return "mvnw.cmd";
        }
        if dir.join("mvnw").is_file() {
            return "mvnw";
        }
    }
    "mvn"
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactorInfo {
    pub root: String,
    /// Module path relative to the reactor root, `/`-separated.
    pub relative_module: String,
}

/// Find the topmost ancestor whose `<modules>` actually lists this module. A parent pom
/// without `<modules>` (the usual shape in this corpus) is *not* a reactor root.
pub fn find_reactor_root(module_dir: &Path, base: &Path) -> Option<ReactorInfo> {
    let module = module_dir.canonicalize().unwrap_or_else(|_| module_dir.to_path_buf());
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let mut best: Option<ReactorInfo> = None;
    let mut current = module.parent()?.to_path_buf();
    let mut hops = 0;

    while hops < 12 {
        hops += 1;
        if !current.starts_with(&base) {
            break;
        }
        if let Some(pom) = read_pom(&current) {
            if pom.is_aggregator() {
                // `child` is the directory on the path from this ancestor toward the module.
                if let Some(child) = immediate_child(&current, &module) {
                    if lists_module(&pom.modules, &child) {
                        best = Some(ReactorInfo {
                            root: path_text(&current),
                            relative_module: relative(&current, &module),
                        });
                        // Keep climbing: the coarsest shared root wins, so `-pl` can
                        // batch several modules of one build later on.
                    }
                }
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    best
}

/// The direct child directory of `ancestor` that is an ancestor-or-self of `target`.
fn immediate_child(ancestor: &Path, target: &Path) -> Option<String> {
    let rest = target.strip_prefix(ancestor).ok()?;
    rest.components()
        .next()
        .map(|part| part.as_os_str().to_string_lossy().to_string())
}

/// `<module>` entries are plain names, paths, or globs like `bundle/*`.
fn lists_module(modules: &[String], child: &str) -> bool {
    let child = child.trim_end_matches('/').replace('\\', "/");
    modules.iter().any(|entry| {
        let entry = entry.trim().trim_end_matches('/').replace('\\', "/");
        if entry.is_empty() {
            return false;
        }
        if entry == child {
            return true;
        }
        if let Some(prefix) = entry.strip_suffix("/*") {
            return child.starts_with(&format!("{}/", prefix.trim_end_matches('/'))) || child == prefix;
        }
        // `<module>sub/deep</module>` still makes `sub` the child on the path.
        entry
            .split('/')
            .next()
            .map(|first| first == child)
            .unwrap_or(false)
    })
}

fn relative(ancestor: &Path, target: &Path) -> String {
    target
        .strip_prefix(ancestor)
        .map(|rest| rest.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Everything `run_test_sync` needs to launch one maven module's tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MavenPlan {
    pub exec_dir: String,
    pub program: String,
    pub args: Vec<String>,
    /// Surefire reports are always under the module, even when the build runs at the
    /// reactor root.
    pub reports_root: String,
    /// The parent pom sets `skipTests=true` in some corpora; without this override the
    /// run "succeeds" having executed nothing.
    pub forces_tests: bool,
}

/// `-pl :artifactId` (with the colon) selects by coordinate instead of path, which is
/// immune to the module directory being named differently from the artifact.
pub fn plan_maven_run(
    module_dir: &Path,
    reactor: Option<&ReactorInfo>,
    artifact_id: &str,
    extra_args: Option<&str>,
) -> MavenPlan {
    let module_dir_text = path_text(module_dir);
    let (exec_dir, mut args) = match reactor {
        Some(info) if !info.root.is_empty() => (
            info.root.clone(),
            vec!["-B".to_string(), "-pl".to_string(), format!(":{}", artifact_id), "-am".to_string()],
        ),
        _ => (module_dir_text.clone(), vec!["-B".to_string()]),
    };
    let exec_path = PathBuf::from(&exec_dir);
    args.push("test".to_string());
    args.push("-DskipTests=false".to_string());
    if let Some(extra) = extra_args.map(str::trim).filter(|e| !e.is_empty()) {
        args.extend(extra.split_whitespace().map(str::to_string));
    }
    MavenPlan {
        forces_tests: true,
        program: maven_launcher(if reactor.is_some() { &exec_path } else { module_dir }).to_string(),
        exec_dir,
        args,
        reports_root: module_dir_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Structural shape of a real leaf module pom: `<parent>` first, tabs, a commented
    /// out `<version>` inside `<parent>`, no own `<groupId>` and no `<packaging>`.
    const LEAF_POM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
	<modelVersion>4.0.0</modelVersion>
	<parent>
		<groupId>com.pmys.saas</groupId>
		<artifactId>pmys.saas.parent.pom</artifactId>
<!--		<version>2.4.338-RELEASE</version>-->
		<version>0.0.5-SNAPSHOT</version>
	</parent>
	<artifactId>pmys.saas.aio.ui</artifactId>
	<version>0.0.1-SNAPSHOT</version>
	<properties>
		<skywalking.version>8.9.0</skywalking.version>
	</properties>
	<dependencies>
		<dependency>
			<groupId>com.pmys.saas</groupId>
			<artifactId>pmys.saas.common.sdk</artifactId>
		</dependency>
	</dependencies>
</project>
"#;

    const PARENT_POM: &str = r#"<project>
    <groupId>com.pmys.saas</groupId>
    <artifactId>pmys.saas.parent.pom</artifactId>
    <version>0.0.5-SNAPSHOT</version>
    <packaging>pom</packaging>
    <parent>
        <groupId>org.springframework.boot</groupId>
        <artifactId>spring-boot-starter-parent</artifactId>
        <version>2.1.13.RELEASE</version>
        <relativePath/> <!-- lookup parent from repository -->
    </parent>
    <properties>
        <java.version>1.8</java.version>
        <skipTests>true</skipTests>
        <pmys.saas.common.sdk.version>0.0.5-SNAPSHOT</pmys.saas.common.sdk.version>
    </properties>
</project>"#;

    const AGGREGATOR_POM: &str = r#"<project>
   <artifactId>pmys.saas.report.service</artifactId>
   <packaging>pom</packaging>
   <modules>
       <module>report-server</module>
       <module>report-sdk</module>
   </modules>
   <properties><maven.compiler.source>8</maven.compiler.source></properties>
</project>"#;

    #[test]
    fn leaf_pom_reads_own_coordinates_and_inherits_the_group() {
        let pom = parse_pom_minimal(LEAF_POM).expect("a leaf pom parses");
        assert_eq!(pom.coord.artifact_id, "pmys.saas.aio.ui");
        assert_eq!(pom.coord.version, "0.0.1-SNAPSHOT", "the commented-out version is ignored");
        assert_eq!(pom.coord.group_id, "com.pmys.saas", "inherited from <parent>");
        assert_eq!(pom.packaging, "jar", "maven's default");
        assert!(!pom.is_aggregator());
        let parent = pom.parent.expect("parent recorded");
        assert_eq!(parent.artifact_id, "pmys.saas.parent.pom");
        assert_eq!(parent.version, "0.0.5-SNAPSHOT");
    }

    #[test]
    fn parent_pom_exposes_the_skip_tests_landmine() {
        let pom = parse_pom_minimal(PARENT_POM).unwrap();
        assert_eq!(pom.packaging, "pom");
        assert!(pom.skip_tests, "<properties><skipTests>true</skipTests> must be visible");
        assert!(!pom.is_aggregator(), "a parent without <modules> is not a reactor");
        assert_eq!(pom.coord.group_id, "com.pmys.saas", "depth-1 groupId, not the spring parent's");
    }

    #[test]
    fn aggregator_lists_its_modules() {
        let pom = parse_pom_minimal(AGGREGATOR_POM).unwrap();
        assert!(pom.is_aggregator());
        assert!(!pom.is_leaf_project());
        assert_eq!(pom.modules, vec!["report-server", "report-sdk"]);
    }

    #[test]
    fn module_matching_covers_names_paths_and_globs() {
        assert!(lists_module(&["report-sdk".to_string()], "report-sdk"));
        // `<module>bundle/*</module>` expands inside `bundle/`, so `bundle` is the child.
        assert!(lists_module(&["bundle/*".to_string()], "bundle"));
        assert!(lists_module(&["sub/deep".to_string()], "sub"));
        assert!(!lists_module(&["other".to_string()], "sub"));
        assert!(!lists_module(&["bundle/*".to_string()], "unrelated"));
    }

    #[test]
    fn a_broken_wrapper_falls_back_to_plain_mvn() {
        let root = std::env::temp_dir().join(format!("aiwb-mvn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let good = root.join("good");
        let broken = root.join("broken");
        fs::create_dir_all(good.join(".mvn/wrapper")).unwrap();
        fs::create_dir_all(&broken).unwrap();
        for dir in [&good, &broken] {
            fs::write(dir.join("mvnw.cmd"), "@echo off\r\n").unwrap();
        }
        fs::write(good.join(".mvn/wrapper/maven-wrapper.properties"), "distributionUrl=x\r\n").unwrap();

        assert!(!is_wrapper_broken(&good));
        assert_eq!(maven_launcher(&good), "mvnw.cmd");
        assert!(is_wrapper_broken(&broken), "wrapper script without .mvn payload");
        assert_eq!(maven_launcher(&broken), "mvn");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reactor_root_needs_modules_that_name_the_child_dir() {
        let root = std::env::temp_dir().join(format!("aiwb-reactor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let parent_only = root.join("p1").join("parent.pom");
        let module = root.join("p1").join("aio.ui");
        fs::create_dir_all(&parent_only).unwrap();
        fs::create_dir_all(&module).unwrap();
        // A parent pom without <modules> must not be mistaken for a reactor root.
        fs::write(parent_only.join("pom.xml"), "<project><artifactId>parent</artifactId><packaging>pom</packaging></project>").unwrap();
        fs::write(module.join("pom.xml"), LEAF_POM).unwrap();
        assert_eq!(find_reactor_root(&module, &root), None);

        // Now a real aggregator two levels up that lists the module directory.
        let agg = root.join("p1");
        fs::write(
            agg.join("pom.xml"),
            "<project><artifactId>agg</artifactId><packaging>pom</packaging><modules><module>aio.ui</module></modules></project>",
        )
        .unwrap();
        let found = find_reactor_root(&module, &root).expect("the aggregator is the root");
        assert!(found.root.ends_with("p1"), "got {}", found.root);
        assert_eq!(found.relative_module, "aio.ui");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verbatim_windows_prefix_never_reaches_the_output() {
        assert_eq!(path_text(Path::new(r"\\?\D:\repo\p1\mod")), "D:/repo/p1/mod");
        assert_eq!(path_text(Path::new(r"D:\repo\p1")), "D:/repo/p1");
        // The reactor root is canonicalized internally, so assert on the real thing.
        let root = std::env::temp_dir().join(format!("aiwb-verbatim-{}", std::process::id()));
        let module = root.join("agg").join("child");
        fs::create_dir_all(&module).unwrap();
        fs::write(
            root.join("agg").join("pom.xml"),
            "<project><artifactId>agg</artifactId><packaging>pom</packaging><modules><module>child</module></modules></project>",
        )
        .unwrap();
        fs::write(module.join("pom.xml"), "<project><artifactId>child</artifactId></project>").unwrap();
        let found = find_reactor_root(&module, &root).expect("agg is the root");
        assert!(!found.root.contains("?"), "verbatim prefix leaked: {}", found.root);
        assert!(!found.root.contains('\\'), "backslash leaked: {}", found.root);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_plan_forces_tests_and_keeps_reports_with_the_module() {
        let module = Path::new("D:/repo/p1/pmys.saas.aio.ui");
        let plain = plan_maven_run(module, None, "pmys.saas.aio.ui", None);
        assert_eq!(plain.exec_dir, "D:/repo/p1/pmys.saas.aio.ui");
        assert_eq!(plain.args, vec!["-B", "test", "-DskipTests=false"]);
        assert_eq!(plain.reports_root, "D:/repo/p1/pmys.saas.aio.ui");

        let reactor = ReactorInfo { root: "D:/repo/p1".to_string(), relative_module: "pmys.saas.aio.ui".to_string() };
        let grouped = plan_maven_run(module, Some(&reactor), "pmys.saas.aio.ui", Some("-- -Dtest=XTest"));
        assert_eq!(grouped.exec_dir, "D:/repo/p1", "the build runs at the reactor root");
        assert_eq!(grouped.args, vec!["-B", "-pl", ":pmys.saas.aio.ui", "-am", "test", "-DskipTests=false", "--", "-Dtest=XTest"]);
        assert_eq!(grouped.reports_root, "D:/repo/p1/pmys.saas.aio.ui", "but the reports stay in the module");
    }
}
