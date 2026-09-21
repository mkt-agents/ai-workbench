//! Turn a change report into "run these tests".
//!
//! Pure: `ChangeReport` in, `TestSelection` out. The output is deliberately two things
//! at once — a list a human can tick, and one ready-made argument string per runner,
//! because every runner spells "only these tests" differently and getting that wrong
//! means silently running the whole suite (or nothing).
//!
//! What it does *not* claim: that a listed target covers the change. Files with no
//! paired test are reported as `gaps` instead of being quietly dropped.

use serde::{Deserialize, Serialize};

use crate::change_report::ChangeReport;

/// How many targets one run is asked to cover. Beyond this the runner's own filter
/// syntax gets unwieldy (and a surefire `-Dtest=` list stops being readable).
pub const DEFAULT_MAX_TARGETS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestTarget {
    /// What will actually be run: a Java class name, a file path, a libtest filter.
    pub name: String,
    /// `class` | `file` | `filter` — decides how the UI labels it.
    pub kind: String,
    /// The changed file that pulled this target in.
    pub from: String,
    /// True when the target itself was part of the change (an edited test).
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSelection {
    pub framework: String,
    pub targets: Vec<TestTarget>,
    /// Changed code files that have no test we can point at.
    pub gaps: Vec<String>,
    /// Append to the project's test command (`run_test`'s `args`).
    pub args: String,
    pub truncated: bool,
    /// `false` when the framework has no filter syntax we trust: run everything.
    pub selectable: bool,
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// `app/src/test/java/com/x/FooTest.java` → `com.x.FooTest`.
fn java_class_from_path(path: &str) -> String {
    for marker in ["/java/", "/kotlin/", "/groovy/"] {
        if let Some((_, rest)) = path.split_once(marker) {
            return rest.trim_end_matches(".java").trim_end_matches(".kt").replace('/', ".");
        }
    }
    file_stem(path)
}

fn push_target(targets: &mut Vec<TestTarget>, seen: &mut Vec<String>, target: TestTarget) {
    if seen.contains(&target.name) {
        return;
    }
    seen.push(target.name.clone());
    targets.push(target);
}

/// Build the selection for one project's change report.
pub fn select_tests(report: &ChangeReport, framework: &str, max: usize) -> TestSelection {
    let mut targets: Vec<TestTarget> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();

    for file in &report.files {
        let is_test = file.layer == "test";
        // An edited test is its own target; an edited source points at its pair.
        let target = match (&file.test_path, is_test) {
            (Some(test_path), _) => test_path.clone(),
            (None, true) => file.path.clone(),
            (None, false) => {
                if is_testable(&file.layer) && !gaps.contains(&file.path) {
                    gaps.push(file.path.clone());
                }
                continue;
            }
        };
        let (name, kind) = match framework {
            "maven" => (java_class_from_path(&target), "class".to_string()),
            "cargo" => (file_stem(&target), "filter".to_string()),
            "pytest" => (target.clone(), "file".to_string()),
            "gotest" => (
                target
                    .rsplit_once('/')
                    .map(|(dir, _)| {
                        format!(
                            "./{}/...",
                            dir.rsplit_once("/src/").map(|(_, r)| r).unwrap_or(dir)
                        )
                    })
                    .unwrap_or_else(|| "./...".to_string()),
                "filter".to_string(),
            ),
            _ => (target.clone(), "file".to_string()),
        };
        if !name.is_empty() {
            push_target(
                &mut targets,
                &mut seen,
                TestTarget {
                    name,
                    kind,
                    from: file.path.clone(),
                    changed: is_test,
                },
            );
        }
    }

    let truncated = targets.len() > max;
    targets.truncate(max);

    let (args, selectable) = args_for(framework, &targets);

    TestSelection {
        framework: framework.to_string(),
        targets,
        gaps,
        args,
        truncated,
        selectable,
    }
}

/// Runner arguments for one framework's target list, plus whether that framework has a
/// filter syntax we trust at all.
pub fn args_for(framework: &str, targets: &[TestTarget]) -> (String, bool) {
    if targets.is_empty() {
        // Even for a filterable framework: an empty list must not become `-Dtest=`.
        return (String::new(), framework_filterable(framework));
    }
    match framework {
        "maven" => (
            format!(
                "-Dtest={} -DfailIfNoTests=false",
                targets
                    .iter()
                    .map(|t| t.name.rsplit('.').next().unwrap_or(&t.name))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            true,
        ),
        // `npm test` swallows unknown flags unless they come after `--`.
        "jest" | "vitest" | "mocha" | "playwright" => (
            format!("-- {}", targets.iter().map(|t| quote(&t.name)).collect::<Vec<_>>().join(" ")),
            true,
        ),
        "cargo" | "pytest" | "gotest" => (
            targets.iter().map(|t| quote(&t.name)).collect::<Vec<_>>().join(" "),
            true,
        ),
        // Anything else: we cannot promise a filter that does not silently run nothing.
        _ => (String::new(), false),
    }
}

fn framework_filterable(framework: &str) -> bool {
    matches!(
        framework,
        "maven" | "jest" | "vitest" | "mocha" | "playwright" | "cargo" | "pytest" | "gotest"
    )
}

/// The same selection restricted to the entries the user left ticked. Names come back
/// from the UI, so unknown ones are dropped instead of reaching the runner.
pub fn subset(selection: &TestSelection, names: &[String]) -> TestSelection {
    let targets: Vec<TestTarget> = selection
        .targets
        .iter()
        .filter(|target| names.iter().any(|name| name == &target.name))
        .cloned()
        .collect();
    let (args, selectable) = args_for(&selection.framework, &targets);
    TestSelection {
        framework: selection.framework.clone(),
        targets,
        gaps: selection.gaps.clone(),
        args,
        truncated: false,
        selectable,
    }
}

fn is_testable(layer: &str) -> bool {
    matches!(
        layer,
        "api" | "service" | "persistence" | "logic" | "model" | "page" | "component" | "view" | "state" | "frontend"
    )
}

/// Paths with spaces must survive the runner's own argv split.
fn quote(value: &str) -> String {
    if value.contains(' ') && !value.starts_with('"') {
        format!("\"{}\"", value)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_report::{build_report, FileChange};
    use std::collections::HashSet;

    fn empty_set() -> HashSet<String> {
        HashSet::new()
    }

    fn report(name_status: &str, numstat: &str, known_tests: &[&str]) -> ChangeReport {
        let known: HashSet<String> = known_tests.iter().map(|p| p.to_lowercase()).collect();
        build_report(
            "",
            "uncommitted",
            vec![],
            name_status,
            numstat,
            "",
            &known,
            &empty_set(),
            "",
            false,
        )
    }

    #[test]
    fn maven_targets_become_a_surefire_class_list() {
        let report = report(
            "M\0app/src/main/java/com/x/web/UserController.java\0M\0app/src/main/java/com/x/service/Orphan.java\0",
            "5\t2\tapp/src/main/java/com/x/web/UserController.java\0",
            &["app/src/test/java/com/x/web/UserControllerTest.java"],
        );
        let selection = select_tests(&report, "maven", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.targets.len(), 1, "got {:?}", selection.targets);
        assert_eq!(selection.targets[0].name, "com.x.web.UserControllerTest");
        assert_eq!(selection.targets[0].kind, "class");
        assert_eq!(
            selection.args,
            "-Dtest=UserControllerTest -DfailIfNoTests=false",
            "simple class names keep the flag readable"
        );
        assert_eq!(selection.gaps, vec!["app/src/main/java/com/x/service/Orphan.java"]);
    }

    #[test]
    fn a_ticked_subset_rebuilds_the_args() {
        let report = report(
            "M\0a/src/main/java/com/x/web/AController.java\0M\0b/src/main/java/com/x/web/BController.java\0",
            "",
            &["a/src/test/java/com/x/web/AControllerTest.java", "b/src/test/java/com/x/web/BControllerTest.java"],
        );
        let all = select_tests(&report, "maven", DEFAULT_MAX_TARGETS);
        assert_eq!(all.targets.len(), 2, "got {:?}", all.targets);
        assert_eq!(all.args, "-Dtest=AControllerTest,BControllerTest -DfailIfNoTests=false");

        let one = subset(&all, &["com.x.web.BControllerTest".to_string()]);
        assert_eq!(one.targets.len(), 1);
        assert_eq!(one.args, "-Dtest=BControllerTest -DfailIfNoTests=false", "only the ticked class runs");
        assert!(one.selectable);
        assert_eq!(one.gaps, all.gaps, "the gaps belong to the change, not to the tick list");

        assert!(subset(&all, &[]).args.is_empty(), "an empty selection must not become -Dtest=");
        assert!(
            subset(&all, &["com.x.web.NotOffered".to_string()]).targets.is_empty(),
            "a name the backend never offered is dropped, not passed to the runner"
        );
    }

    #[test]
    fn a_changed_test_file_targets_itself() {
        let report = report(
            "M\0app/src/test/java/com/x/web/UserControllerTest.java\0",
            "1\t1\tapp/src/test/java/com/x/web/UserControllerTest.java\0",
            &["app/src/test/java/com/x/web/UserControllerTest.java"],
        );
        let selection = select_tests(&report, "maven", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.targets[0].name, "com.x.web.UserControllerTest");
        assert!(selection.targets[0].changed);
        assert!(selection.gaps.is_empty(), "a test change is not a gap");
    }

    #[test]
    fn js_runners_need_the_double_dash_and_the_file_paths() {
        let report = report(
            "M\0src/core/store.ts\0M\0src/core/store.test.ts\0",
            "",
            &["src/core/store.test.ts"],
        );
        let selection = select_tests(&report, "vitest", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.targets[0].name, "src/core/store.test.ts");
        assert!(selection.args.starts_with("-- "), "{}", selection.args);
        assert!(selection.args.contains("src/core/store.test.ts"));
        // `store.ts` has no paired test of its own beyond the one already listed.
        assert!(selection.selectable);
    }

    #[test]
    fn cargo_filters_use_the_module_stem_and_pytest_uses_paths() {
        let cargo = report("M\0src-tauri/src/git_cache.rs\0", "", &["src-tauri/tests/git_cache.rs"]);
        let selection = select_tests(&cargo, "cargo", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.args, "git_cache");

        let pytest = report("M\0pkg/service.py\0", "", &["tests/test_service.py"]);
        let selection = select_tests(&pytest, "pytest", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.args, "tests/test_service.py");
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let report = report("M\0src/pages/用户 管理.vue\0", "", &["src/pages/用户 管理.test.ts"]);
        let selection = select_tests(&report, "vitest", DEFAULT_MAX_TARGETS);
        assert_eq!(selection.targets[0].name, "src/pages/用户 管理.test.ts");
        assert!(selection.args.contains("\"src/pages/用户 管理.test.ts\""), "{}", selection.args);
    }

    #[test]
    fn the_target_list_is_capped_and_says_so() {
        let mut name_status = String::new();
        let mut known = Vec::new();
        for index in 0..25 {
            name_status.push_str(&format!("M\0app/src/main/java/com/x/C{}.java\0", index));
            known.push(format!("app/src/test/java/com/x/C{}Test.java", index));
        }
        let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
        let selection = select_tests(&report(&name_status, "", &known_refs), "maven", 20);
        assert_eq!(selection.targets.len(), 20);
        assert!(selection.truncated);
        assert_eq!(selection.args.matches(',').count(), 19);
    }

    #[test]
    fn an_unknown_framework_offers_no_filter_instead_of_a_wrong_one() {
        let report = report("M\0weird/src/main.x\0", "", &[]);
        let selection = select_tests(&report, "custom", DEFAULT_MAX_TARGETS);
        assert!(selection.args.is_empty());
        assert!(!selection.selectable);
    }

    #[test]
    fn docs_and_config_changes_are_not_test_gaps() {
        let report = report("M\0README.md\0M\0src/main/resources/application.yml\0", "", &[]);
        let selection = select_tests(&report, "maven", DEFAULT_MAX_TARGETS);
        assert!(selection.gaps.is_empty(), "got {:?}", selection.gaps);
        assert!(selection.targets.is_empty());
        assert_eq!(selection.args, "");
    }

    #[test]
    fn a_file_change_without_any_test_candidate_is_a_gap() {
        let file = FileChange {
            path: "svc/src/main/java/com/x/Big.java".to_string(),
            old_path: None,
            status: "M".to_string(),
            adds: 40,
            dels: 1,
            module: "svc".to_string(),
            layer: "service".to_string(),
            risks: vec![],
            test_path: None,
            has_test: false,
            untracked: false,
        };
        assert!(is_testable(&file.layer));
    }
}
