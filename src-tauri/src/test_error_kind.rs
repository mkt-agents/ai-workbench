//! Why did a run fail, beyond the five-value status?
//!
//! `success|failed|error|cancelled|timeout` answers *that* something went wrong; this
//! module answers *what* went wrong, so a list of eight 「错误」 chips can say
//! 「缺少依赖」 three times, 「pom 跳过测试」 twice and 「无测试源码」 once.
//!
//! Pure: patterns over the captured output plus read-only filesystem probes.

use std::path::{Path, PathBuf};

/// Stable tokens; the UI translates them with `t(\`errorKind.${kind}\`)`.
/// Only referenced by the tests that assert the vocabulary stays closed.
#[allow(dead_code)]
pub const KINDS: [&str; 9] = [
    "dependency",
    "wrapper",
    "compile",
    "noTests",
    "skippedByPom",
    "timeout",
    "cancelled",
    "command",
    "unknown",
];

/// A registered project that could provide a missing artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub group_id: String,
    pub artifact_id: String,
    pub path: String,
}

/// `aborted` is `"" | "cancelled" | "timeout"` from the runner; `total` is the counted
/// number of test cases; `has_test_sources` lets "no test sources at all" be told apart
/// from "tests existed but were skipped".
pub fn classify(
    output: &str,
    framework: &str,
    build_ok: bool,
    total: u32,
    aborted: &str,
    has_test_sources: bool,
) -> &'static str {
    if aborted == "cancelled" {
        return "cancelled";
    }
    if aborted == "timeout" {
        return "timeout";
    }
    if total > 0 {
        // Something ran; the counts already describe the outcome.
        return "unknown";
    }
    if is_skipped_by_pom(output) {
        return "skippedByPom";
    }
    if let Some(kind) = tool_failure_kind(output, framework) {
        return kind;
    }
    if !build_ok {
        return "unknown";
    }
    if has_test_sources {
        // The build passed and tests existed, yet nothing was counted: the runner's
        // output/report never reached us.
        return "unknown";
    }
    "noTests"
}

/// Surefire's own words when `skipTests` is on: the build succeeds and prints this
/// instead of any `Tests run:` line — verified against real Maven 3.9.6 output.
fn is_skipped_by_pom(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("tests are skipped")
        || lower.contains("tests are skipped.")
        || lower.contains("surefire:*-test (default-test) @") && lower.contains("skipping test execution")
}

fn tool_failure_kind(output: &str, framework: &str) -> Option<&'static str> {
    let lower = output.to_lowercase();
    if lower.contains("mavenwrappermain") || lower.contains("couldn't find \"") && lower.contains("maven-wrapper") {
        return Some("wrapper");
    }
    let dependency_markers = [
        "could not resolve dependencies",
        "the following artifacts could not be resolved",
        "the pom for ",
        "could not find artifact",
        "failure to find",
        "npm err! 404",
        "eresolve",
        "no matching version",
        "failed to get `",
        "failed to download",
        "moduleerror",
        "no module named",
        "cannot find package",
        "could not select method",
    ];
    if dependency_markers.iter().any(|marker| lower.contains(marker)) {
        return Some("dependency");
    }
    let compile_markers = [
        "compilation error",
        "maven-compiler-plugin",
        "cannot find symbol",
        "error[c",
        "could not compile",
        "error: expected",
        "undefined: ",
        "syntaxerror",
        "typeerror: ",
        "failed to compile",
        "build failed",
        "error: linking",
        "unresolved reference",
    ];
    if compile_markers.iter().any(|marker| lower.contains(marker)) {
        return Some("compile");
    }
    match framework {
        "cargo" if lower.contains("no such file or directory") => Some("command"),
        _ => None,
    }
}

/// `The POM for com.pmys.saas:pmys.saas.common.sdk:jar:0.0.5-SNAPSHOT is missing` and
/// `… could not be resolved: com.g:a:jar:v (absent)` both yield `(g, a, v)`.
///
/// Only the text *after* an anchor phrase is scanned: the same maven line also names
/// the project's own coordinate (`for project com.x:mine:jar:1.0`), which is not a
/// missing artifact.
pub fn missing_artifacts(output: &str) -> Vec<(String, String, String)> {
    const ANCHORS: [&str; 4] = [
        "the pom for ",
        "could not be resolved:",
        "could not find artifact ",
        "failed to read artifact ",
    ];
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in output.lines() {
        let lower = line.to_lowercase();
        for anchor in ANCHORS {
            let Some(at) = lower.find(anchor) else { continue };
            let tail = &line[at + anchor.len()..];
            for word in tail.split_whitespace() {
                let token = word.trim_end_matches(|c: char| {
                    matches!(c, ',' | '.' | ':' | ';' | ')' | '(' | '[' | ']' | '"')
                });
                let parts: Vec<&str> = token.split(':').collect();
                // group:artifact:jar:version (or …:pom:version)
                if parts.len() == 4 && (parts[2] == "jar" || parts[2] == "pom") {
                    let entry = (parts[0].to_string(), parts[1].to_string(), parts[3].to_string());
                    if !entry.0.contains('/') && !out.contains(&entry) {
                        out.push(entry);
                    }
                }
            }
        }
    }
    out
}

/// The `<localRepository>` that actually applies: the last uncommented one wins, which
/// is how Maven reads `settings.xml` too.
pub fn settings_local_repo(home: &Path) -> Option<PathBuf> {
    let content = fs_text(&home.join(".m2").join("settings.xml"))?;
    let stripped = strip_xml_comments(&content);
    let mut found: Option<String> = None;
    for chunk in stripped.split("<localRepository>").skip(1) {
        if let Some(end) = chunk.find("</localRepository>") {
            let value = chunk[..end].trim();
            if !value.is_empty() {
                found = Some(value.to_string());
            }
        }
    }
    found.map(PathBuf::from)
}

fn fs_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn strip_xml_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(offset) => rest = &rest[start + offset + 3..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// Versions present in the local repository for one artifact. A directory that only
/// holds `.lastUpdated` markers means a failed download, which is a different answer
/// from "installed" — and the distinction is what the user needs.
pub fn installed_versions(repo: &Path, group: &str, artifact: &str) -> Vec<(String, bool)> {
    let dir = repo.join(group.replace('.', "/")).join(artifact);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let version = entry.file_name().to_string_lossy().to_string();
        let has_artifact = std::fs::read_dir(entry.path())
            .map(|files| {
                files.flatten().any(|f| {
                    let name = f.file_name().to_string_lossy().to_lowercase();
                    (name.ends_with(".jar") || name.ends_with(".pom")) && !name.ends_with(".lastUpdated")
                })
            })
            .unwrap_or(false);
        out.push((version, has_artifact));
    }
    out.sort();
    out
}

/// Human-readable evidence lines appended to the run output. Nothing here is executed —
/// the point is that the user can copy a command instead of guessing.
pub fn diagnose(
    output: &str,
    kind: &str,
    local_repo: Option<&Path>,
    providers: &[Provider],
) -> Vec<String> {
    let mut lines = Vec::new();
    match kind {
        "skippedByPom" => lines.push(
            "[诊断] 测试被 pom/属性里的 skipTests 跳过：构建成功但没有执行任何用例。运行命令已自动带 -DskipTests=false；若仍出现此条，说明是命令行参数或 profile 显式跳过。"
                .to_string(),
        ),
        "wrapper" => lines.push(
            "[诊断] mvnw 包装器缺少 .mvn/wrapper/maven-wrapper.jar，无法自举。已改用系统 mvn；如需修复，执行 mvn -N wrapper:wrapper 重新生成。"
                .to_string(),
        ),
        "noTests" => lines.push(
            "[诊断] 该模块没有测试源码目录（src/test 下无 .java/.kt），本次运行不可能产生用例。"
                .to_string(),
        ),
        "dependency" => {
            for (group, artifact, version) in missing_artifacts(output).into_iter().take(6) {
                let mut line = format!("[诊断] 缺少依赖 {}:{}:{}", group, artifact, version);
                if let Some(repo) = local_repo {
                    let versions = installed_versions(repo, &group, &artifact);
                    line.push_str(&match versions.len() {
                        0 => format!("（本地仓库 {} 里没有该构件）", repo.display()),
                        1 if versions[0].1 => format!("（本地仓库只有 {}）", versions[0].0),
                        1 => format!("（本地仓库有 {} 目录，但只有下载失败标记，没有 jar/pom）", versions[0].0),
                        n => format!("（本地仓库有 {} 个版本：{}）", n, versions.iter().map(|(v, ok)| format!("{}{}", v, if *ok { "" } else { "(空)" })).collect::<Vec<_>>().join(", ")),
                    });
                }
                if let Some(provider) = providers
                    .iter()
                    .find(|p| p.group_id == group && p.artifact_id == artifact)
                {
                    line.push_str(&format!(
                        "；已登记工程里有源码：{}，可在该目录执行 mvn -B install -DskipTests 后重试",
                        provider.path
                    ));
                }
                lines.push(line);
            }
        }
        _ => {}
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of the six tct failures captured from the app database.
    const DEPENDENCY: &str = "[WARNING] The POM for com.pmys.saas:pmys.saas.common.sdk:jar:0.0.5-SNAPSHOT is missing, no dependency information available\n\
[INFO] ------------------------------------------------------------------------\n\
[ERROR] Failed to execute goal on project pmys.saas.aio.ui: Could not resolve dependencies for project com.pmys.saas:pmys.saas.aio.ui:jar:0.0.1-SNAPSHOT: The following artifacts could not be resolved: com.pmys.saas:pmys.saas.common.sdk:jar:0.0.5-SNAPSHOT (absent)\n\
[ERROR] [Help 1] http://cwiki.apache.org/confluence/display/MAVEN/DependencyResolutionException\n\
[INFO] BUILD FAILURE\n";

    /// Real Maven 3.9.6 output for a pom with `<skipTests>true</skipTests>`.
    const SKIPPED: &str = "[INFO] --- surefire:3.1.2:test (default-test) @ probe-maven ---\n[INFO] Tests are skipped.\n[INFO] ------------------------------------------------------------------------\n[INFO] BUILD SUCCESS\n";

    const WRAPPER: &str = "Couldn't find \"D:\\repo\\.mvn\\wrapper\\maven-wrapper.jar\", downloading it ...\n\
java.lang.ClassNotFoundException: org.apache.maven.wrapper.MavenWrapperMain\n[ERROR] Failed to execute goal\n";

    fn kind_of(output: &str) -> &'static str {
        classify(output, "maven", false, 0, "", true)
    }

    #[test]
    fn real_maven_dependency_failure_is_classified_and_parsed() {
        assert_eq!(kind_of(DEPENDENCY), "dependency");
        let missing = missing_artifacts(DEPENDENCY);
        assert!(
            missing.contains(&("com.pmys.saas".to_string(), "pmys.saas.common.sdk".to_string(), "0.0.5-SNAPSHOT".to_string())),
            "got {:?}",
            missing
        );
    }

    #[test]
    fn a_green_build_that_ran_nothing_is_skipped_by_pom_not_no_tests() {
        assert_eq!(classify(SKIPPED, "maven", true, 0, "", true), "skippedByPom");
        assert_eq!(classify(SKIPPED, "maven", true, 0, "", false), "skippedByPom", "the skip marker wins over the no-sources guess");
    }

    #[test]
    fn a_broken_wrapper_is_its_own_kind() {
        assert_eq!(kind_of(WRAPPER), "wrapper");
    }

    #[test]
    fn build_success_without_any_test_source_is_no_tests() {
        assert_eq!(
            classify("[INFO] BUILD SUCCESS\n", "maven", true, 0, "", false),
            "noTests"
        );
        assert_eq!(
            classify("[INFO] BUILD SUCCESS\n", "maven", true, 0, "", true),
            "unknown",
            "tests existed but produced nothing: do not claim there are none"
        );
    }

    #[test]
    fn cancellation_and_timeout_beat_everything_else() {
        assert_eq!(classify(DEPENDENCY, "maven", false, 0, "cancelled", true), "cancelled");
        assert_eq!(classify(DEPENDENCY, "maven", false, 0, "timeout", true), "timeout");
    }

    #[test]
    fn a_run_that_executed_cases_needs_no_kind() {
        assert_eq!(classify(DEPENDENCY, "maven", false, 12, "", true), "unknown");
    }

    #[test]
    fn other_ecosystems_map_onto_the_same_kinds() {
        assert_eq!(classify("error: could not compile `app`\n", "cargo", false, 0, "", true), "compile");
        assert_eq!(
            classify("error: failed to get `serde` of registered packages\n", "cargo", false, 0, "", true),
            "dependency"
        );
        assert_eq!(classify("npm ERR! code ERESOLVE\n", "vitest", false, 0, "", true), "dependency");
        assert_eq!(classify("ModuleNotFoundError: No module named 'x'\n", "pytest", false, 0, "", true), "dependency");
        assert_eq!(classify("?   my/pkg   [no test files]\n", "gotest", true, 0, "", false), "noTests");
        assert_eq!(
            classify("cannot find package \"example.com/x\"\n", "gotest", false, 0, "", true),
            "dependency"
        );
    }

    #[test]
    fn settings_local_repo_ignores_the_commented_example() {
        let home = std::env::temp_dir().join(format!("aiwb-m2-{}", std::process::id()));
        let dir = home.join(".m2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.xml"),
            "<settings>\n  <!-- localRepository\n  <localRepository>/path/to/local/repo</localRepository>\n  -->\n  <localRepository>D:\\d_mvn\\spring-cloud</localRepository>\n</settings>",
        )
        .unwrap();
        assert_eq!(
            settings_local_repo(&home),
            Some(PathBuf::from("D:\\d_mvn\\spring-cloud"))
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_empty_version_directory_is_reported_as_a_failed_download() {
        let repo = std::env::temp_dir().join(format!("aiwb-repo-{}", std::process::id()));
        let good = repo.join("com/x/saas").join("thing.sdk").join("1.0.0");
        let empty = repo.join("com/x/saas").join("thing.sdk").join("2.0.0");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(good.join("thing.sdk-1.0.0.jar"), "x").unwrap();
        std::fs::write(empty.join("thing.sdk-2.0.0.pom.lastUpdated"), "x").unwrap();

        let versions = installed_versions(&repo, "com.x.saas", "thing.sdk");
        assert_eq!(versions, vec![("1.0.0".to_string(), true), ("2.0.0".to_string(), false)]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn the_dependency_diagnosis_names_the_version_the_repo_actually_has() {
        let repo = std::env::temp_dir().join(format!("aiwb-diag-{}", std::process::id()));
        let only = repo.join("com/pmys/saas").join("pmys.saas.common.sdk").join("2.0.42-RELEASE");
        std::fs::create_dir_all(&only).unwrap();
        std::fs::write(only.join("pmys.saas.common.sdk-2.0.42-RELEASE.pom.lastUpdated"), "x").unwrap();

        let providers = vec![Provider {
            group_id: "com.pmys.saas".to_string(),
            artifact_id: "pmys.saas.common.sdk".to_string(),
            path: "D:/d_project/tct/p1/pmys.saas.common.sdk".to_string(),
        }];
        let lines = diagnose(DEPENDENCY, "dependency", Some(&repo), &providers);
        assert_eq!(lines.len(), 1, "got {:?}", lines);
        let text = &lines[0];
        assert!(text.contains("pmys.saas.common.sdk:0.0.5-SNAPSHOT"), "{}", text);
        assert!(text.contains("2.0.42-RELEASE"), "{}", text);
        assert!(text.contains("下载失败标记"), "{}", text);
        assert!(text.contains("p1/pmys.saas.common.sdk"), "{}", text);
        assert!(text.contains("mvn -B install -DskipTests"), "{}", text);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn kinds_are_a_closed_vocabulary() {
        assert_eq!(classify("totally new failure\n", "maven", false, 0, "", true), "unknown");
        assert!(KINDS.contains(&classify(DEPENDENCY, "maven", false, 0, "", true)));
    }
}
