//! Turning raw test-runner output into structured results.
//!
//! Kept free of tauri/sqlite on purpose: this module is pure text -> data, so it can
//! be compiled and tested outside the app binary (`cargo test` in the app crate, or by
//! including this file from a scratch crate when the app's own test binary cannot run).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestCase {
    pub id: String,
    pub name: String,
    pub status: String,
    pub duration: u64,
    pub error: Option<TestCaseError>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestCaseError {
    pub message: String,
    pub stack: String,
    pub expected: Option<serde_json::Value>,
    pub actual: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestSuite {
    pub name: String,
    pub path: String,
    pub status: String,
    pub duration: u64,
    pub tests: Vec<TestCase>,
}

/// Read the integer that sits in front of a summary keyword ("5 passed" → 5).
pub fn num_before(chunk: &str, keyword: &str) -> u32 {
    chunk
        .split(keyword)
        .next()
        .and_then(|s| s.split_whitespace().last())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// (status, total, passed, failed, skipped) from a runner's human-readable output.
pub fn parse_test_output(output: &str, framework: &str) -> (String, u32, u32, u32, u32) {
    let mut total = 0u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;

    match framework {
        "jest" | "vitest" => {
            // Summary line wins: "Tests  5 passed | 1 failed (6)" (vitest) or
            // "Tests:  5 passed, 1 failed (6)" (jest). Also counting per-test
            // checkmarks on top of a summary double-counts verbose reporters.
            for line in output.lines() {
                if let Some(part) = line.split("Tests").nth(1) {
                    passed += num_before(part, "passed");
                    failed += num_before(part, "failed");
                    skipped += num_before(part, "skipped");
                }
            }

            if passed + failed + skipped == 0 {
                // No summary at all (aborted or crashed mid-run): fall back to marks.
                for line in output.lines() {
                    passed += line.matches('✓').count() as u32 + line.matches('√').count() as u32;
                    failed += line.matches('✗').count() as u32 + line.matches('×').count() as u32;
                    skipped += line.matches('○').count() as u32;
                }
            }

            total = passed + failed + skipped;
        }
        "cargo" => {
            // One "test result:" line per test binary:
            // "test result: ok. 12 passed; 0 failed; 0 ignored; …"
            for line in output.lines() {
                if line.contains("test result:") {
                    if line.contains("passed") {
                        passed += num_before(line, "passed");
                    }
                    if line.contains("failed") {
                        failed += num_before(line, "failed");
                    }
                    if line.contains("ignored") {
                        skipped += num_before(line, "ignored");
                    }
                }
                // "running 12 tests" — the count sits between the two words.
                if let Some(pos) = line.find("running ") {
                    total += num_before(&line[pos + "running ".len()..], "test");
                }
            }
            if total == 0 {
                total = passed + failed + skipped;
            }
        }
        "pytest" => {
            // pytest prints exactly one summary line at the very end:
            // "== 5 passed, 1 failed, 2 skipped, 1 warning in 0.42s =="
            // Scanning every line that mentions "passed" also catches coverage
            // tables and echoed commands, which inflates the counts.
            if let Some(line) = output
                .lines()
                .rev()
                .find(|l| l.contains("passed") || l.contains("failed") || l.contains("error"))
            {
                passed += num_before(line, "passed");
                failed += num_before(line, "failed");
                skipped += num_before(line, "skipped");
                // Collection errors are reported as "1 error" and mean the run broke.
                failed += num_before(line, "error");
            }
            total = passed + failed + skipped;
        }
        "gotest" => {
            // Per-test markers only ("--- PASS: TestX (0.00s)"). The package-level
            // "ok"/"PASS" lines would add one more pass per package.
            for line in output.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("--- PASS:") {
                    passed += 1;
                } else if trimmed.starts_with("--- FAIL:") {
                    failed += 1;
                } else if trimmed.starts_with("--- SKIP:") {
                    skipped += 1;
                }
            }
            total = passed + failed + skipped;
        }
        _ => {
            // Generic parsing
            for line in output.lines() {
                if line.contains("pass") || line.contains("✓") || line.contains("√") {
                    passed += 1;
                }
                if line.contains("fail") || line.contains("✗") || line.contains("×") {
                    failed += 1;
                }
                if line.contains("skip") || line.contains("○") {
                    skipped += 1;
                }
            }
            total = passed + failed + skipped;
        }
    }

    let status = if failed > 0 {
        "failed".to_string()
    } else if total == 0 {
        "error".to_string()
    } else {
        "success".to_string()
    };

    (status, total, passed, failed, skipped)
}

/// Where a jest/vitest JSON reporter is expected to write, relative to the project
/// root. Users opt in with `--reporter=json --outputFile=vitest-results.json`.
pub const RESULT_FILE_CANDIDATES: &[&str] = &[
    "vitest-results.json",
    "jest-results.json",
    "test-results.json",
    ".ai-workbench/results.json",
];

/// Parse a Jest-compatible machine report (jest `--json` and vitest's `json`
/// reporter emit the same shape) into per-file suites with per-case detail.
pub fn parse_jest_style_results(content: &str) -> Result<Vec<TestSuite>, String> {
    let json: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("解析测试结果 JSON 失败: {}", e))?;
    let results = json
        .get("testResults")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "测试结果 JSON 缺少 testResults 数组".to_string())?;

    let mut suites = Vec::new();
    for (index, entry) in results.iter().enumerate() {
        let path = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut cases = Vec::new();
        if let Some(assertions) = entry.get("assertionResults").and_then(|v| v.as_array()) {
            for (case_index, assertion) in assertions.iter().enumerate() {
                let raw_status = assertion.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let status = match raw_status {
                    "passed" => "passed",
                    "failed" => "failed",
                    _ => "skipped",
                };
                let name = assertion
                    .get("fullName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        let mut parts: Vec<String> = assertion
                            .get("ancestorTitles")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        parts.push(
                            assertion
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        );
                        Some(parts.join(" > "))
                    })
                    .unwrap_or_default();
                let duration = assertion
                    .get("duration")
                    .and_then(|v| v.as_f64())
                    .filter(|d| *d > 0.0)
                    .map(|d| d.round() as u64)
                    .unwrap_or(0);
                let failures: Vec<String> = assertion
                    .get("failureMessages")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let error = if status == "failed" && !failures.is_empty() {
                    Some(error_from_text(&failures.join("\n\n")))
                } else {
                    None
                };
                cases.push(TestCase {
                    id: format!("{}#{}", path, case_index),
                    name: if name.is_empty() {
                        format!("case {}", case_index + 1)
                    } else {
                        name
                    },
                    status: status.to_string(),
                    duration,
                    error,
                });
            }
        }
        let failed = cases.iter().filter(|c| c.status == "failed").count();
        suites.push(TestSuite {
            name: basename(&path).unwrap_or_else(|| format!("suite {}", index + 1)),
            path,
            status: if failed > 0 { "failed" } else { "passed" }.to_string(),
            duration: cases.iter().map(|c| c.duration).sum(),
            tests: cases,
        });
    }
    if suites.iter().all(|s| s.tests.is_empty()) {
        return Err("报告里没有任何用例".to_string());
    }
    Ok(suites)
}

/// (total, passed, failed, skipped) straight from a parsed report.
pub fn suite_counts(suites: &[TestSuite]) -> (u32, u32, u32, u32) {
    let cases: Vec<&TestCase> = suites.iter().flat_map(|s| s.tests.iter()).collect();
    let passed = cases.iter().filter(|c| c.status == "passed").count() as u32;
    let failed = cases.iter().filter(|c| c.status == "failed").count() as u32;
    let skipped = cases.iter().filter(|c| c.status == "skipped").count() as u32;
    (cases.len() as u32, passed, failed, skipped)
}

/// Locate a machine-readable report: a conventional file first, then stdout when it
/// is a bare JSON document (jest/vitest print the report alone with `--json`).
pub fn load_structured_suites(root: &Path, stdout: &str, stderr: &str) -> Option<Vec<TestSuite>> {
    for candidate in RESULT_FILE_CANDIDATES {
        let file = root.join(candidate);
        if let Ok(content) = fs::read_to_string(&file) {
            if let Ok(suites) = parse_jest_style_results(&content) {
                return Some(suites);
            }
        }
    }
    for stream in [stdout, stderr] {
        let trimmed = stream.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        if let Ok(suites) = parse_jest_style_results(trimmed) {
            return Some(suites);
        }
    }
    None
}

fn basename(path: &str) -> Option<String> {
    path.rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Split a failure blob into a one-line message and the remaining stack.
fn error_from_text(text: &str) -> TestCaseError {
    let text = text.trim();
    let (message, stack) = match text.split_once('\n') {
        Some((first, rest)) => (first.trim().to_string(), rest.trim().to_string()),
        None => (text.to_string(), String::new()),
    };
    TestCaseError {
        message,
        stack,
        expected: None,
        actual: None,
    }
}

fn close_suite(suites: &mut Vec<TestSuite>, current: Option<TestSuite>) {
    if let Some(mut suite) = current {
        if suite.tests.is_empty() {
            return;
        }
        let failed = suite.tests.iter().filter(|c| c.status == "failed").count();
        suite.status = if failed > 0 { "failed" } else { "passed" }.to_string();
        suite.duration = suite.tests.iter().map(|c| c.duration).sum();
        suites.push(suite);
    }
}

fn new_suite(name: String, path: String) -> TestSuite {
    TestSuite {
        name,
        path,
        status: "passed".to_string(),
        duration: 0,
        tests: Vec::new(),
    }
}

/// Collect `---- name stdout ----` blocks (cargo) so a failing case carries its panic.
fn capture_blocks(output: &str, prefix: &str, suffix: &str) -> HashMap<String, String> {
    let mut blocks: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    let mut buffer = String::new();
    for line in output.lines() {
        if let Some(head) = line.strip_prefix(prefix) {
            if let Some(rest) = head.strip_suffix(suffix) {
                if let Some(name) = current.take() {
                    blocks.insert(name, buffer.trim().to_string());
                }
                current = Some(rest.trim().to_string());
                buffer = String::new();
                continue;
            }
        }
        match &mut current {
            Some(_) => {
                buffer.push_str(line);
                buffer.push('\n');
            }
            None => {}
        }
    }
    if let Some(name) = current {
        blocks.insert(name, buffer.trim().to_string());
    }
    blocks
}

/// `cargo test` per-case detail straight from stdout: no extra flags needed.
pub fn parse_cargo_cases(output: &str) -> Vec<TestSuite> {
    let failures = capture_blocks(output, "---- ", " stdout ----");
    let mut suites: Vec<TestSuite> = Vec::new();
    let mut current: Option<TestSuite> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        // "Running unittests src/lib.rs (target\debug\deps\crate-1a2b.exe)" — the
        // source path is the token that looks like a file, not the "unittests" label.
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            close_suite(&mut suites, current.take());
            let source = rest
                .split_whitespace()
                .find(|token| token.ends_with(".rs") || token.contains('/'))
                .unwrap_or("")
                .replace('\\', "/");
            current = Some(new_suite(basename(&source).unwrap_or(source.clone()), source));
            continue;
        }
        // "test some::case ... ok"
        if let Some(rest) = trimmed.strip_prefix("test ") {
            if let Some((name, tail)) = rest.split_once(" ... ") {
                let status = match tail.trim() {
                    "ok" => "passed",
                    "FAILED" => "failed",
                    "ignored" => "skipped",
                    _ => continue,
                };
                let name = name.trim().to_string();
                let error = if status == "failed" {
                    failures.get(&name).map(|text| error_from_text(text))
                } else {
                    None
                };
                let case = TestCase {
                    id: name.clone(),
                    name: name.clone(),
                    status: status.to_string(),
                    duration: 0,
                    error,
                };
                let suite = current.get_or_insert_with(|| new_suite("cargo test".to_string(), String::new()));
                suite.tests.push(case);
            }
        }
    }
    close_suite(&mut suites, current.take());
    suites
}

/// `pytest -v` per-case detail: `tests/test_a.py::test_x PASSED   [ 20%]`.
pub fn parse_pytest_cases(output: &str) -> Vec<TestSuite> {
    // The short summary carries the assertion message per failing node id.
    let mut messages: HashMap<String, String> = HashMap::new();
    for line in output.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("FAILED ")
            .or_else(|| trimmed.strip_prefix("ERROR "))
        {
            if let Some((node, reason)) = rest.split_once(" - ") {
                messages.insert(node.trim().to_string(), reason.trim().to_string());
            }
        }
    }

    let mut suites: Vec<TestSuite> = Vec::new();
    let mut current: Option<TestSuite> = None;
    for line in output.lines() {
        let Some(node_pos) = line.find(".py::") else { continue };
        let file = line[..node_pos + 3].trim().to_string();
        // After `.py::` comes one whitespace-free node id (`Class::test` allowed),
        // then the status word: `TestRound::test_round PASSED  [ 50%]`.
        let Some(test_path) = line[node_pos + 5..].split_whitespace().next() else {
            continue;
        };
        let Some(word) = line[node_pos + 5..].split_whitespace().nth(1) else {
            continue;
        };
        let status = match word {
            "PASSED" => "passed",
            "FAILED" | "ERROR" => "failed",
            "SKIPPED" | "XFAIL" | "XPASS" => "skipped",
            _ => continue,
        };
        let node_id = format!("{}::{}", file, test_path);
        let name = test_path.rsplit("::").next().unwrap_or(&test_path).to_string();
        let error = if status == "failed" {
            messages
                .get(&node_id)
                .or_else(|| messages.get(node_id.rsplit("::").next().unwrap_or("")))
                .map(|text| error_from_text(text))
        } else {
            None
        };

        let matches_current = current
            .as_ref()
            .map(|s| s.path == file)
            .unwrap_or(false);
        if !matches_current {
            close_suite(&mut suites, current.take());
            current = Some(new_suite(
                basename(&file).unwrap_or_else(|| file.clone()),
                file,
            ));
        }
        if let Some(suite) = current.as_mut() {
            suite.tests.push(TestCase {
                id: node_id,
                name,
                status: status.to_string(),
                duration: 0,
                error,
            });
        }
    }
    close_suite(&mut suites, current.take());
    suites
}

/// `go test -v` per-case detail: `--- PASS: TestX (0.00s)`.
pub fn parse_gotest_cases(output: &str) -> Vec<TestSuite> {
    let mut suite = new_suite("go test".to_string(), String::new());
    for line in output.lines() {
        let trimmed = line.trim_start();
        let (status, rest) = if let Some(r) = trimmed.strip_prefix("--- PASS:") {
            ("passed", r)
        } else if let Some(r) = trimmed.strip_prefix("--- FAIL:") {
            ("failed", r)
        } else if let Some(r) = trimmed.strip_prefix("--- SKIP:") {
            ("skipped", r)
        } else {
            continue;
        };
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        // "--- FAIL: TestX (0.12s)" — the duration sits in the trailing parens.
        let duration = rest
            .rsplit_once('(')
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .map(|value| value.trim_end_matches(')').trim_end_matches('s'))
            .and_then(|secs| secs.parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .map(|s| (s * 1000.0).round() as u64)
            .unwrap_or(0);
        suite.tests.push(TestCase {
            id: name.clone(),
            name,
            status: status.to_string(),
            duration,
            error: None,
        });
    }
    if suite.tests.is_empty() {
        Vec::new()
    } else {
        suite.status = if suite.tests.iter().any(|c| c.status == "failed") {
            "failed".to_string()
        } else {
            "passed".to_string()
        };
        suite.duration = suite.tests.iter().map(|c| c.duration).sum();
        vec![suite]
    }
}

/// Per-case detail for runners that only print human-readable output.
pub fn line_parsed_suites(framework: &str, output: &str) -> Option<Vec<TestSuite>> {
    let suites = match framework {
        "cargo" => parse_cargo_cases(output),
        "pytest" => parse_pytest_cases(output),
        "gotest" => parse_gotest_cases(output),
        _ => Vec::new(),
    };
    (!suites.is_empty()).then_some(suites)
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    const CARGO: &str = r#"
     Running unittests src/lib.rs (target\debug\deps\app-1a2b3c.exe)

running 3 tests
test tests::add ... ok
test tests::divide ... FAILED
test tests::flaky ... ignored

failures:

---- tests::divide stdout ----

thread 'tests::divide' panicked at src/lib.rs:24:9:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1`

failures:
    tests::divide

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/main.rs (target\debug\deps\app-4d5e6f.exe)

running 1 test
test smoke::boot ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
"#;

    #[test]
    fn cargo_output_becomes_suites_and_cases() {
        let suites = parse_cargo_cases(CARGO);
        assert_eq!(suites.len(), 2, "one suite per `Running` binary");
        assert_eq!(suites[0].name, "lib.rs");
        assert_eq!(suites[0].path, "src/lib.rs");
        assert_eq!(suites[0].status, "failed");
        assert_eq!(suites[0].tests.len(), 3);
        assert_eq!(suites[1].name, "main.rs");
        assert_eq!(suites[1].tests.len(), 1);
        assert_eq!(suite_counts(&suites), (4, 2, 1, 1));

        let failing = &suites[0].tests[1];
        assert_eq!(failing.name, "tests::divide");
        let err = failing.error.as_ref().expect("the panic text is attached");
        assert!(err.message.contains("panicked at src/lib.rs:24:9"), "{}", err.message);
        assert!(err.stack.contains("left: 1"), "{}", err.stack);
        assert_eq!(suites[0].tests[2].status, "skipped");
    }

    const PYTEST: &str = r#"
============================= test session starts ==============================
collected 4 items

tests/test_math.py::test_add PASSED                                     [ 25%]
tests/test_math.py::TestRound::test_round PASSED                        [ 50%]
tests/test_math.py::test_divide FAILED                                  [ 75%]
tests/test_web.py::test_health SKIPPED (needs server)                   [100%]

=================================== FAILURES ===================================
___________________________ test_divide ____________________________

    def test_divide():
>       assert divide(2, 1) == 2
E       assert 2 == 1

=========================== short test summary info ============================
FAILED tests/test_math.py::test_divide - assert 2 == 1
==================== 1 failed, 2 passed, 1 skipped in 0.42s ====================
"#;

    #[test]
    fn pytest_verbose_output_becomes_suites_per_file() {
        let suites = parse_pytest_cases(PYTEST);
        assert_eq!(suites.len(), 2);
        assert_eq!(suites[0].name, "test_math.py");
        assert_eq!(suites[0].tests.len(), 3);
        assert_eq!(suites[0].status, "failed");
        assert_eq!(suites[1].name, "test_web.py");
        assert_eq!(suite_counts(&suites), (4, 2, 1, 1));

        // Class-qualified node ids keep the method name only.
        assert_eq!(suites[0].tests[1].name, "test_round");
        assert_eq!(suites[0].tests[1].id, "tests/test_math.py::TestRound::test_round");

        let failing = &suites[0].tests[2];
        let err = failing.error.as_ref().expect("the short summary line is attached");
        assert_eq!(err.message, "assert 2 == 1");
    }

    #[test]
    fn gotest_markers_become_cases_with_durations() {
        let output = "=== RUN   TestAdd\n--- PASS: TestAdd (0.12s)\n=== RUN   TestDiv\n\
                      --- FAIL: TestDiv (0.5s)\n--- SKIP: TestFlaky (0.00s)\nFAIL\n";
        let suites = parse_gotest_cases(output);
        assert_eq!(suites.len(), 1);
        assert_eq!(suites[0].status, "failed");
        assert_eq!(
            suites[0]
                .tests
                .iter()
                .map(|c| (c.name.as_str(), c.status.as_str(), c.duration))
                .collect::<Vec<_>>(),
            vec![
                ("TestAdd", "passed", 120),
                ("TestDiv", "failed", 500),
                ("TestFlaky", "skipped", 0)
            ]
        );
    }

    #[test]
    fn line_parsing_is_off_for_runners_with_a_json_report_and_for_unknown_frameworks() {
        assert!(line_parsed_suites("cargo", CARGO).is_some());
        assert!(line_parsed_suites("pytest", PYTEST).is_some());
        assert!(line_parsed_suites("vitest", "Tests  1 passed (1)").is_none());
        assert!(line_parsed_suites("cargo", "no cases at all").is_none());
    }
}
