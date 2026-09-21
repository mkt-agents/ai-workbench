//! Turning raw test-runner output into structured results.
//!
//! Kept free of tauri/sqlite on purpose: this module is pure text -> data, so it can
//! be compiled and tested outside the app binary (`cargo test` in the app crate, or by
//! including this file from a scratch crate when the app's own test binary cannot run).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
        "maven" => {
            // Surefire prints one line per test class
            // `[INFO] Tests run: 2, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.3 s -- in com.x.T`
            // plus one aggregate per module with no `- in` suffix. Summing both
            // double-counts, so the per-class lines win when they are there.
            let mut class_totals = [0u32; 4];
            let mut aggregate_totals = [0u32; 4];
            let mut saw_class_line = false;
            for line in output.lines() {
                let Some((_, rest)) = line.split_once("Tests run:") else {
                    continue;
                };
                let total = int_after(rest, "").unwrap_or(0);
                let line_failed = int_after(rest, "Failures").unwrap_or(0)
                    + int_after(rest, "Errors").unwrap_or(0);
                let line_skipped = int_after(rest, "Skipped").unwrap_or(0);
                let totals = [
                    total,
                    total.saturating_sub(line_failed + line_skipped),
                    line_failed,
                    line_skipped,
                ];
                let target = if line.contains("- in ") {
                    saw_class_line = true;
                    &mut class_totals
                } else {
                    &mut aggregate_totals
                };
                for (slot, value) in target.iter_mut().zip(totals) {
                    *slot += value;
                }
            }
            let [t, p, f, s] = if saw_class_line {
                class_totals
            } else {
                aggregate_totals
            };
            total = t;
            passed = p;
            failed = f;
            skipped = s;
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

/// The digits that sit right after a keyword (`"Skipped: 2,"` → 2). An empty keyword
/// reads from the start of the slice.
fn int_after(hay: &str, keyword: &str) -> Option<u32> {
    let tail = if keyword.is_empty() {
        hay
    } else {
        hay.split_once(keyword)?.1
    };
    let digits: String = tail
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
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
        // maven: stdout only carries per-class totals, so it has no case detail.
        // Surefire's XML reports are the sole source — see `load_surefire_suites`.
        _ => Vec::new(),
    };
    (!suites.is_empty()).then_some(suites)
}

// --------------------------------------------------------------------------- maven

/// Surefire/failsafe write one XML per test class into `<module>/target/surefire-reports`.
const REPORT_DIRS: &[&str] = &["surefire-reports", "failsafe-reports"];
/// Bound the walk: a big monorepo has thousands of directories under a reactor root.
const MAX_REPORT_FILES: usize = 500;
const MAX_REPORT_DEPTH: usize = 6;
/// Never searched — either irrelevant or the place reports live in.
const PRUNED_DIRS: &[&str] = &[".git", "node_modules", ".gradle", "src", "dist", "build"];

/// Every `TEST-*.xml` under `root`, shallowest first, capped.
pub fn find_surefire_reports(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_surefire_reports(root, 0, &mut out);
    out
}

fn collect_surefire_reports(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_REPORT_DEPTH || out.len() >= MAX_REPORT_FILES {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let path = entry.path();
        if REPORT_DIRS.contains(&name.as_str()) {
            if let Ok(files) = fs::read_dir(&path) {
                for file in files.flatten() {
                    let fname = file.file_name().to_string_lossy().to_string();
                    if fname.starts_with("TEST-") && fname.ends_with(".xml") && out.len() < MAX_REPORT_FILES {
                        out.push(file.path());
                    }
                }
            }
            continue;
        }
        if PRUNED_DIRS.contains(&name.as_str()) {
            continue;
        }
        collect_surefire_reports(&path, depth + 1, out);
    }
}

/// Case detail straight from surefire reports — the only exact maven source, since
/// reactor stdout loses per-class detail across modules.
///
/// `not_before` drops reports from earlier runs: maven never cleans `target/`, so
/// without it a deleted test class keeps showing up in every later run.
pub fn load_surefire_suites(root: &Path, not_before: Option<SystemTime>) -> Option<Vec<TestSuite>> {
    let mut suites: Vec<TestSuite> = Vec::new();
    for file in find_surefire_reports(root) {
        if let Some(start) = not_before {
            let fresh = fs::metadata(&file)
                .and_then(|m| m.modified())
                .map(|mtime| mtime >= start)
                .unwrap_or(true); // no mtime available: keep the report
            if !fresh {
                continue;
            }
        }
        let content = match fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };
        if let Ok(parsed) = parse_surefire_xml(&content) {
            for mut suite in parsed {
                if suite.path.is_empty() {
                    suite.path = file.display().to_string();
                }
                suites.push(suite);
            }
        }
    }
    (!suites.is_empty()).then_some(suites)
}

/// One tag of a deliberately small XML reader: surefire writes a narrow dialect, and
/// pulling in an XML crate for it would cost more than the ~100 lines below.
/// `coverage_parsers` reads jacoco and cobertura with the same reader.
#[derive(Debug)]
pub(crate) struct XmlTag<'a> {
    pub name: &'a str,
    pub attrs: Vec<(&'a str, &'a str)>,
    pub close: bool,
    pub empty: bool,
    pub end: usize,
}

pub(crate) fn next_xml_tag(s: &str, from: usize) -> Option<XmlTag<'_>> {
    let bytes = s.as_bytes();
    let mut i = from;
    loop {
        let byte = *bytes.get(i)?;
        if byte != b'<' {
            i += 1;
            continue;
        }
        let rest = &s[i..];
        if let Some(tail) = rest.strip_prefix("<!--") {
            i += 4 + tail.find("-->")? + 3;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("<![CDATA[") {
            i += 9 + tail.find("]]>")? + 3;
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i += 2 + rest[2..].find('>')? + 1;
            continue;
        }
        break;
    }

    let mut j = i + 1;
    let close = s[j..].starts_with('/');
    if close {
        j += 1;
    }
    let name_start = j;
    while j < bytes.len() && !matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') {
        j += 1;
    }
    let name = &s[name_start..j];
    if name.is_empty() {
        return None;
    }

    let mut attrs: Vec<(&str, &str)> = Vec::new();
    let mut empty = false;
    loop {
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        if bytes[j] == b'/' && s[j..].starts_with("/>") {
            empty = true;
            j += 2;
            break;
        }
        if bytes[j] == b'>' {
            j += 1;
            break;
        }
        let key_start = j;
        while j < bytes.len() && !matches!(bytes[j], b'=' | b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            return None; // malformed or bare word: stop rather than mis-read the file
        }
        let key = &s[key_start..j];
        j += 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        let quote = *bytes.get(j)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = j + 1;
        let offset = s[value_start..].find(quote as char)?;
        attrs.push((key, &s[value_start..value_start + offset]));
        j = value_start + offset + 1;
    }

    Some(XmlTag {
        name,
        attrs,
        close,
        empty,
        end: j,
    })
}

pub(crate) fn attr<'a>(attrs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn seconds_to_ms(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .map(|v| (v * 1000.0).round() as u64)
        .unwrap_or(0)
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find(';') {
            Some(j) if j > 0 && j <= 10 => {
                let entity = &tail[..j];
                let decoded = match entity {
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "amp" => Some('&'),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                        .and_then(char::from_u32),
                };
                match decoded {
                    Some(c) => {
                        out.push(c);
                        rest = &tail[j + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = tail;
                    }
                }
            }
            _ => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Strip a CDATA wrapper and normalise line endings inside report text.
fn report_text(raw: &str) -> String {
    let body = raw.trim();
    let body = match body.strip_prefix("<![CDATA[") {
        Some(tail) => tail.rsplit_once("]]>").map_or(tail, |(head, _)| head),
        None => body,
    };
    decode_entities(body.trim()).replace("\r\n", "\n")
}

/// `<failure>`/`<error>` carry the assertion message as an attribute and the stack as
/// the element text; either can be missing.
fn failure_error(message: Option<&str>, body: &str) -> TestCaseError {
    let stack = report_text(body);
    let message = message.map(decode_entities).unwrap_or_default();
    let combined = match (message.trim(), stack.trim()) {
        ("", rest) => rest.to_string(),
        (head, "") => head.to_string(),
        (head, rest) => format!("{}\n{}", head, rest),
    };
    error_from_text(&combined)
}

fn case_outcome(body: &str) -> (&'static str, Option<TestCaseError>) {
    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(body, pos) {
        pos = tag.end;
        if tag.close {
            continue;
        }
        match tag.name {
            "skipped" => {
                let message = attr(&tag.attrs, "message");
                let error = message.map(|m| error_from_text(&decode_entities(m)));
                return ("skipped", error);
            }
            "failure" | "error" => {
                let close = format!("</{}>", tag.name);
                let text = match body[tag.end..].find(&close) {
                    Some(offset) => &body[tag.end..tag.end + offset],
                    None => &body[tag.end..],
                };
                return (
                    "failed",
                    Some(failure_error(attr(&tag.attrs, "message"), text)),
                );
            }
            other => {
                // system-out / system-err / rerunFailure: jump past the element so its
                // captured output is never mistaken for a failure.
                if !tag.empty {
                    let close = format!("</{}>", other);
                    if let Some(offset) = body[pos..].find(&close) {
                        pos += offset + close.len();
                    }
                }
            }
        }
    }
    ("passed", None)
}

/// Parse one surefire report. Both the single-`<testsuite>` file surefire writes and
/// the `<testsuites>` wrapper other JUnit reporters produce are accepted.
pub fn parse_surefire_xml(content: &str) -> Result<Vec<TestSuite>, String> {
    let mut suites: Vec<TestSuite> = Vec::new();
    let mut current: Option<TestSuite> = None;
    let mut case_no = 0usize;
    let mut pos = 0usize;

    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        match (tag.name, tag.close) {
            ("testsuite", false) => {
                if !tag.empty {
                    current = Some(TestSuite {
                        name: attr(&tag.attrs, "name").unwrap_or("suite").to_string(),
                        path: String::new(),
                        status: "passed".to_string(),
                        duration: seconds_to_ms(attr(&tag.attrs, "time")),
                        tests: Vec::new(),
                    });
                }
            }
            ("testsuite", true) => {
                if let Some(suite) = current.take() {
                    suites.push(finalize_suite(suite));
                }
            }
            ("testcase", false) | ("rerunTestcase", false) => {
                let close = format!("</{}>", tag.name);
                let body = if tag.empty {
                    ""
                } else {
                    match content[pos..].find(&close) {
                        Some(offset) => {
                            let head = &content[pos..pos + offset];
                            pos += offset + close.len();
                            head
                        }
                        None => &content[pos..],
                    }
                };
                let (status, error) = case_outcome(body);
                let name = attr(&tag.attrs, "name").unwrap_or("").to_string();
                let class = attr(&tag.attrs, "classname").unwrap_or("");
                case_no += 1;
                let duration = seconds_to_ms(attr(&tag.attrs, "time"));
                current
                    .get_or_insert_with(|| TestSuite {
                        name: "testcase".to_string(),
                        path: String::new(),
                        status: "passed".to_string(),
                        duration: 0,
                        tests: Vec::new(),
                    })
                    .tests
                    .push(TestCase {
                        id: format!("{}#{}", class, case_no),
                        name: if name.is_empty() {
                            format!("case {}", case_no)
                        } else {
                            name
                        },
                        status: status.to_string(),
                        duration,
                        error,
                    });
            }
            _ => {}
        }
    }
    if let Some(suite) = current.take() {
        suites.push(finalize_suite(suite));
    }

    suites.retain(|s| !s.tests.is_empty());
    if suites.is_empty() {
        return Err("surefire 报告里没有任何用例".to_string());
    }
    Ok(suites)
}

fn finalize_suite(mut suite: TestSuite) -> TestSuite {
    let failed = suite.tests.iter().filter(|c| c.status == "failed").count();
    suite.status = if failed > 0 { "failed" } else { "passed" }.to_string();
    if suite.duration == 0 {
        suite.duration = suite.tests.iter().map(|c| c.duration).sum();
    }
    suite
}

#[cfg(test)]
mod parser_tests {
    use super::*;
    use std::time::Duration;

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

    // Trimmed from a real surefire 3.0 report, including the `<properties>` block
    // whose attribute values carry backslashes and a bare `>`.
    const SUREFIRE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" version="3.0" name="com.referral.AdminPasswordHashTest" time="0.318" tests="3" errors="0" skipped="1" failures="1">
  <properties>
    <property name="java.class.path" value="D:\repo\target\classes;D:\maven\spring.jar"/>
    <property name="sun.java.command" value="org.apache.maven.surefire.booter.ForkedBooter -- grep a>b"/>
  </properties>
  <testcase name="seedHashMatchesAdmin123" classname="com.referral.AdminPasswordHashTest" time="0.122">
    <system-out><![CDATA[Standard Commons Logging discovery in action with spring-jcl
]]></system-out>
  </testcase>
  <testcase name="oldSeedHashDoesNotMatchAdmin123" classname="com.referral.AdminPasswordHashTest" time="0.07">
    <failure message="expected: &lt;2&gt; but was: &lt;1&gt;" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: expected: <2> but was: <1>
	at com.referral.AdminPasswordHashTest.oldSeedHash(AdminPasswordHashTest.java:31)
]]></failure>
    <system-out><![CDATA[near miss]]></system-out>
  </testcase>
  <testcase name="futureCase" classname="com.referral.AdminPasswordHashTest" time="0">
    <skipped message="disabled on windows"/>
  </testcase>
</testsuite>
"#;

    #[test]
    fn surefire_report_becomes_suites_and_cases() {
        let suites = parse_surefire_xml(SUREFIRE).expect("a surefire 3.0 report parses");
        assert_eq!(suites.len(), 1);
        assert_eq!(suites[0].name, "com.referral.AdminPasswordHashTest");
        assert_eq!(suites[0].status, "failed");
        assert_eq!(suites[0].duration, 318, "`time` is seconds");
        assert_eq!(suite_counts(&suites), (3, 1, 1, 1));

        assert_eq!(suites[0].tests[0].status, "passed");
        assert_eq!(suites[0].tests[0].duration, 122);

        let failing = &suites[0].tests[1];
        let err = failing.error.as_ref().expect("the failure is attached");
        assert_eq!(err.message, "expected: <2> but was: <1>", "entities decoded");
        assert!(err.stack.contains("at com.referral.AdminPasswordHashTest"), "{}", err.stack);
        assert!(!err.stack.contains("near miss"), "system-out is not a stack");

        assert_eq!(suites[0].tests[2].status, "skipped");
        assert_eq!(suites[0].tests[2].duration, 0);
    }

    #[test]
    fn surefire_accepts_a_testsuites_wrapper_and_reports_empty_files() {
        let suites = parse_surefire_xml(
            "<testsuites><testsuite name=\"a.B\" time=\"1\"><testcase name=\"x\" classname=\"a.B\" time=\"0.5\"/></testsuite>\
             <testsuite name=\"a.C\"><testcase name=\"y\" classname=\"a.C\"/></testsuite></testsuites>",
        )
        .expect("both suites parse");
        assert_eq!(suites.len(), 2);
        assert_eq!(suite_counts(&suites), (2, 2, 0, 0));
        assert_eq!(suites[1].duration, 0);

        assert!(parse_surefire_xml("<testsuite name=\"a.B\" tests=\"0\"/>").is_err());
    }

    const MAVEN: &str = "[INFO]  T E S T S\n\
        [INFO] Running com.referral.AdminPasswordHashTest\n\
        [ERROR] Tests run: 3, Failures: 1, Errors: 0, Skipped: 1, Time elapsed: 0.318 s <<< FAILURE! -- in com.referral.AdminPasswordHashTest\n\
        [ERROR] com.referral.AdminPasswordHashTest.oldSeedHash  Time elapsed: 0.07 s  <<< failure!\n\
        org.opentest4j.AssertionFailedError: expected: <2> but was: <1>\n\
        [INFO] Tests run: 2, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.01 s -- in com.referral.OtherTest\n\
        [INFO] Results:\n\
        [INFO] Tests run: 5, Failures: 1, Errors: 0, Skipped: 1\n\
        [INFO] BUILD FAILURE\n";

    #[test]
    fn maven_stdout_counts_per_class_lines_once() {
        assert_eq!(
            parse_test_output(MAVEN, "maven"),
            ("failed".to_string(), 5, 3, 1, 1),
            "the aggregate line must not be added on top of the per-class lines"
        );
    }

    #[test]
    fn maven_stdout_falls_back_to_aggregate_lines_and_no_tests_is_an_error() {
        assert_eq!(
            parse_test_output("[INFO] Tests run: 8, Failures: 0, Errors: 1, Skipped: 2\n", "maven"),
            ("failed".to_string(), 8, 5, 1, 2)
        );
        assert_eq!(
            parse_test_output("[INFO] BUILD FAILURE\n[ERROR] no tests were run\n", "maven").0,
            "error"
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiwb-parser-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_report(dir: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn surefire_reports_are_discovered_per_module_and_stale_ones_dropped() {
        let root = scratch("surefire");
        let one = write_report(
            &root.join("account.service").join("target").join("surefire-reports"),
            "TEST-com.x.AccountTest.xml",
            r#"<testsuite name="com.x.AccountTest"><testcase name="opens" classname="com.x.AccountTest"/></testsuite>"#,
        );
        fs::write(
            root.join("account.service")
                .join("target")
                .join("surefire-reports")
                .join("com.x.AccountTest.txt"),
            "human readable",
        )
        .unwrap();
        write_report(
            &root.join("p2").join("match.service").join("target").join("failsafe-reports"),
            "TEST-com.x.MatchIT.xml",
            r#"<testsuite name="com.x.MatchIT"><testcase name="finds" classname="com.x.MatchIT"/></testsuite>"#,
        );
        // Not a report location, and pruned from the walk.
        write_report(
            &root.join("node_modules").join("pkg").join("target").join("surefire-reports"),
            "TEST-decoy.xml",
            r#"<testsuite name="decoy"><testcase name="x" classname="decoy"/></testsuite>"#,
        );

        let found = find_surefire_reports(&root);
        assert_eq!(found.len(), 2, "one per module, .txt side files ignored: {:?}", found);
        assert!(found.contains(&one));

        let suites = load_surefire_suites(&root, None).expect("both reports parse");
        assert_eq!(suite_counts(&suites), (2, 2, 0, 0));
        assert!(suites[0].path.ends_with(".xml"), "the report file names the suite");

        // Maven never cleans target/, so yesterday's report must not count today.
        let day_old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 - 86_400);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&one)
            .unwrap()
            .set_modified(day_old)
            .unwrap();
        let suites = load_surefire_suites(&root, Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
            .expect("the fresh module still reports");
        assert_eq!(suites.len(), 1);
        assert_eq!(suites[0].name, "com.x.MatchIT");
        let _ = fs::remove_dir_all(&root);
    }
}
