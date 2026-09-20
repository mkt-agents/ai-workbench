use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::DbState;

/// Test-assistant tables, executed by `lib.rs` at startup and by the tests here.
pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS test_projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    type TEXT NOT NULL,
    framework TEXT NOT NULL,
    test_command TEXT NOT NULL,
    args TEXT,
    working_dir TEXT,
    env TEXT,
    enabled INTEGER DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_run_at TEXT,
    last_status TEXT
);
CREATE TABLE IF NOT EXISTS test_runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    status TEXT NOT NULL,
    total_tests INTEGER NOT NULL,
    passed INTEGER NOT NULL,
    failed INTEGER NOT NULL,
    skipped INTEGER NOT NULL,
    output TEXT NOT NULL,
    suites TEXT NOT NULL,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS test_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    run_id TEXT,
    timestamp TEXT NOT NULL,
    status TEXT NOT NULL,
    total INTEGER,
    passed INTEGER,
    failed INTEGER,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
"#;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TestProject {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub project_type: String,
    pub framework: String,
    pub test_command: String,
    pub args: Option<String>,
    pub working_dir: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
}

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunResult {
    pub project_id: String,
    pub id: String,
    pub started_at: String,
    pub completed_at: String,
    pub duration_ms: u64,
    pub status: String,
    pub total_tests: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub output: String,
    pub suites: Vec<TestSuite>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestHistoryEntry {
    pub id: u32,
    pub project_id: String,
    pub run_id: Option<String>,
    pub timestamp: String,
    pub status: String,
    pub total: Option<u32>,
    pub passed: Option<u32>,
    pub failed: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetectionResult {
    pub path: String,
    pub detected: bool,
    pub project_type: Option<String>,
    pub framework: Option<String>,
    pub test_command: Option<String>,
    pub reason: String,
}

/// Read the integer that sits in front of a summary keyword ("5 passed" → 5).
fn num_before(chunk: &str, keyword: &str) -> u32 {
    chunk
        .split(keyword)
        .next()
        .and_then(|s| s.split_whitespace().last())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// SQLite stores blank strings where the domain wants "unset".
fn opt_text(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

/// Keep the tail of a long log — the failing summary is at the end.
fn truncate_tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…（已省略前 {} 字节）…\n{}", start, &text[start..])
}

/// Parse test output and extract test results
fn parse_test_output(output: &str, framework: &str) -> (String, u32, u32, u32, u32) {
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
const RESULT_FILE_CANDIDATES: &[&str] = &[
    "vitest-results.json",
    "jest-results.json",
    "test-results.json",
    ".ai-workbench/results.json",
];

/// Parse a Jest-compatible machine report (jest `--json` and vitest's `json`
/// reporter emit the same shape) into per-file suites with per-case detail.
pub(crate) fn parse_jest_style_results(content: &str) -> Result<Vec<TestSuite>, String> {
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
                    let joined = failures.join("\n\n");
                    let message = joined.lines().next().unwrap_or("").trim().to_string();
                    let stack = joined
                        .split_once('\n')
                        .map(|(_, rest)| rest.trim().to_string())
                        .filter(|rest| !rest.is_empty())
                        .unwrap_or_else(|| joined.clone());
                    Some(TestCaseError {
                        message,
                        stack,
                        expected: None,
                        actual: None,
                    })
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
        let basename = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        suites.push(TestSuite {
            name: if basename.is_empty() {
                format!("suite {}", index + 1)
            } else {
                basename
            },
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
pub(crate) fn suite_counts(suites: &[TestSuite]) -> (u32, u32, u32, u32) {
    let cases: Vec<&TestCase> = suites.iter().flat_map(|s| s.tests.iter()).collect();
    let passed = cases.iter().filter(|c| c.status == "passed").count() as u32;
    let failed = cases.iter().filter(|c| c.status == "failed").count() as u32;
    let skipped = cases.iter().filter(|c| c.status == "skipped").count() as u32;
    (cases.len() as u32, passed, failed, skipped)
}

/// Locate a machine-readable report: a conventional file first, then stdout when it
/// is a bare JSON document (jest/vitest print the report alone with `--json`).
fn load_structured_suites(root: &Path, stdout: &str, stderr: &str) -> Option<Vec<TestSuite>> {
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

/// Load all test projects from database
#[tauri::command]
pub fn load_test_projects(state: State<DbState>) -> Result<Vec<TestProject>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    load_projects_sync(&conn)
}

pub(crate) fn load_projects_sync(conn: &rusqlite::Connection) -> Result<Vec<TestProject>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, path, type, framework, test_command, args, working_dir, env, enabled, created_at, updated_at, last_run_at, last_status FROM test_projects ORDER BY updated_at DESC"
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let projects = stmt
        .query_map([], |row| {
            Ok(TestProject {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                project_type: row.get(3)?,
                framework: row.get(4)?,
                test_command: row.get(5)?,
                args: opt_text(row.get(6)?),
                working_dir: opt_text(row.get(7)?),
                env: row.get::<_, Option<String>>(8)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                enabled: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                last_run_at: opt_text(row.get(12)?),
                last_status: opt_text(row.get(13)?),
            })
        })
        .map_err(|e| format!("Failed to query projects: {}", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse projects: {}", e))?;

    Ok(projects)
}

/// Add a new test project
#[tauri::command]
pub fn add_test_project(
    state: State<DbState>,
    project: TestProject,
) -> Result<TestProject, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    add_project_sync(&conn, project)
}

pub(crate) fn add_project_sync(
    conn: &rusqlite::Connection,
    project: TestProject,
) -> Result<TestProject, String> {
    let env_json = project
        .env
        .as_ref()
        .map(|e| serde_json::to_string(e))
        .transpose()
        .map_err(|e| format!("Failed to serialize env: {}", e))?;

    conn.execute(
        "INSERT INTO test_projects (id, name, path, type, framework, test_command, args, working_dir, env, enabled, created_at, updated_at, last_run_at, last_status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            project.id,
            project.name,
            project.path,
            project.project_type,
            project.framework,
            project.test_command,
            project.args.as_deref().unwrap_or(""),
            project.working_dir.as_deref().unwrap_or(""),
            env_json.as_deref().unwrap_or("{}"),
            if project.enabled { 1 } else { 0 },
            project.created_at,
            project.updated_at,
            project.last_run_at.as_deref().unwrap_or(""),
            project.last_status.as_deref().unwrap_or(""),
        ]
    ).map_err(|e| format!("Failed to insert project: {}", e))?;

    Ok(project)
}

/// Update an existing test project
#[tauri::command]
pub fn update_test_project(
    state: State<DbState>,
    id: String,
    updates: serde_json::Value,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    update_project_sync(&conn, id, updates)
}

pub(crate) fn update_project_sync(
    conn: &rusqlite::Connection,
    id: String,
    updates: serde_json::Value,
) -> Result<(), String> {
    let mut set_clauses = Vec::new();
    let mut params = Vec::new();

    if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
        set_clauses.push("name = ?");
        params.push(name.to_string());
    }
    if let Some(path) = updates.get("path").and_then(|v| v.as_str()) {
        set_clauses.push("path = ?");
        params.push(path.to_string());
    }
    if let Some(project_type) = updates.get("type").and_then(|v| v.as_str()) {
        set_clauses.push("type = ?");
        params.push(project_type.to_string());
    }
    if let Some(framework) = updates.get("framework").and_then(|v| v.as_str()) {
        set_clauses.push("framework = ?");
        params.push(framework.to_string());
    }
    if let Some(test_command) = updates.get("testCommand").and_then(|v| v.as_str()) {
        set_clauses.push("test_command = ?");
        params.push(test_command.to_string());
    }
    if let Some(args) = updates.get("args").and_then(|v| v.as_str()) {
        set_clauses.push("args = ?");
        params.push(args.to_string());
    }
    if let Some(working_dir) = updates.get("workingDir").and_then(|v| v.as_str()) {
        set_clauses.push("working_dir = ?");
        params.push(working_dir.to_string());
    }
    if let Some(env) = updates.get("env") {
        if env.is_object() {
            set_clauses.push("env = ?");
            params.push(env.to_string());
        }
    }
    if let Some(enabled) = updates.get("enabled").and_then(|v| v.as_bool()) {
        set_clauses.push("enabled = ?");
        params.push((if enabled { 1 } else { 0 }).to_string());
    }

    set_clauses.push("updated_at = ?");
    params.push(chrono::Utc::now().to_rfc3339());

    if set_clauses.is_empty() {
        return Ok(());
    }

    params.push(id);

    let sql = format!(
        "UPDATE test_projects SET {} WHERE id = ?",
        set_clauses.join(", ")
    );

    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(|e| format!("Failed to update project: {}", e))?;

    Ok(())
}

/// Delete a test project
#[tauri::command]
pub fn delete_test_project(state: State<DbState>, id: String) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    delete_project_sync(&conn, &id)
}

pub(crate) fn delete_project_sync(conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM test_projects WHERE id = ?", [id])
        .map_err(|e| format!("Failed to delete project: {}", e))?;

    Ok(())
}

/// Detect project type and testing framework from directory
#[tauri::command]
pub fn detect_project_type(path: String) -> Result<ProjectDetectionResult, String> {
    let project_path = Path::new(&path);

    if !project_path.exists() {
        return Ok(ProjectDetectionResult {
            path,
            detected: false,
            project_type: None,
            framework: None,
            test_command: None,
            reason: "none".to_string(),
        });
    }

    // Check for package.json (Node.js/TypeScript projects)
    let package_json = project_path.join("package.json");
    if package_json.exists() {
        let content = fs::read_to_string(&package_json)
            .map_err(|e| format!("Failed to read package.json: {}", e))?;

        let json: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse package.json: {}", e))?;

        // Check for test scripts
        if let Some(scripts) = json.get("scripts").and_then(|v| v.as_object()) {
            if scripts.contains_key("test") {
                let framework = if json.get("devDependencies").and_then(|v| v.get("vitest")).is_some() {
                    Some("vitest".to_string())
                } else if json.get("devDependencies").and_then(|v| v.get("@playwright/test")).is_some() {
                    Some("playwright".to_string())
                } else if json.get("dependencies").and_then(|v| v.get("vitest")).is_some() {
                    Some("vitest".to_string())
                } else if json.get("devDependencies").and_then(|v| v.get("@testing-library/react")).is_some() {
                    Some("jest".to_string())
                } else if json.get("devDependencies").and_then(|v| v.get("mocha")).is_some() {
                    Some("mocha".to_string())
                } else if json.get("devDependencies").and_then(|v| v.get("jest")).is_some() {
                    Some("jest".to_string())
                } else {
                    Some("jest".to_string())
                };

                return Ok(ProjectDetectionResult {
                    path,
                    detected: true,
                    project_type: Some("frontend".to_string()),
                    framework,
                    test_command: Some("npm test".to_string()),
                    reason: "package.json + test script".to_string(),
                });
            }
        }

        return Ok(ProjectDetectionResult {
            path,
            detected: false,
            project_type: Some("frontend".to_string()),
            framework: None,
            test_command: None,
            reason: "package.json".to_string(),
        });
    }

    // Check for Cargo.toml (Rust projects)
    let cargo_toml = project_path.join("Cargo.toml");
    if cargo_toml.exists() {
        return Ok(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("rust".to_string()),
            framework: Some("cargo".to_string()),
            test_command: Some("cargo test".to_string()),
            reason: "Cargo.toml".to_string(),
        });
    }

    // Check for requirements.txt or pytest.ini (Python projects)
    if project_path.join("requirements.txt").exists()
        || project_path.join("pytest.ini").exists()
        || project_path.join("pyproject.toml").exists()
    {
        return Ok(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("python".to_string()),
            framework: Some("pytest".to_string()),
            test_command: Some("pytest".to_string()),
            reason: "pyproject.toml / setup.py / requirements.txt".to_string(),
        });
    }

    // Check for go.mod (Go projects)
    if project_path.join("go.mod").exists() {
        return Ok(ProjectDetectionResult {
            path,
            detected: true,
            project_type: Some("go".to_string()),
            framework: Some("gotest".to_string()),
            test_command: Some("go test ./...".to_string()),
            reason: "go.mod".to_string(),
        });
    }

    Ok(ProjectDetectionResult {
        path,
        detected: false,
        project_type: None,
        framework: None,
        test_command: None,
        reason: "none".to_string(),
    })
}

/// Scan a directory for test projects recursively
#[tauri::command]
pub async fn scan_test_projects(base_path: String) -> Result<Vec<ProjectDetectionResult>, String> {
    let mut results = Vec::new();
    let base = Path::new(&base_path);

    if !base.exists() {
        return Ok(results);
    }

    let entries = fs::read_dir(base)
        .map_err(|e| format!("Failed to read directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();

        if path.is_dir() {
            // Try to detect project type in this directory
            let path_str = path.to_string_lossy().to_string();
            if let Ok(result) = detect_project_type(path_str) {
                if result.detected {
                    results.push(result);
                }
            }
        }
    }

    Ok(results)
}

/// How long one test run may take before its process tree is killed.
const TEST_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Stored output is capped so a chatty suite cannot grow test_runs without bound:
/// 64KB is well past what the panel shows, and the full log stays in the terminal.
const MAX_STORED_OUTPUT_BYTES: usize = 64 * 1024;
/// Newest rows kept per project; older runs and history rows are pruned on write.
const RUN_RETENTION_PER_PROJECT: i64 = 100;
const HISTORY_RETENTION_PER_PROJECT: i64 = 100;

/// Run tests for a project
#[tauri::command]
pub async fn run_test(
    app: AppHandle,
    state: State<'_, DbState>,
    project_id: String,
    args: Option<String>,
) -> Result<TestRunResult, String> {
    let projects = load_test_projects(state.clone())?;
    let project = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project not found: {}", project_id))?;
    let conn = Arc::clone(&state.conn);
    // A suite can run for minutes; keep it off the async worker threads.
    tokio::task::spawn_blocking(move || {
        run_test_sync(conn, project, project_id, args, TEST_RUN_TIMEOUT, Some(app))
    })
    .await
    .map_err(|e| format!("测试任务已中止: {}", e))?
}

/// The directory a run happens in: the UI documents `working_dir` as project-relative
/// ("packages/web"), so resolve it against the project root instead of passing it to
/// the OS verbatim.
fn work_dir_of(project: &TestProject) -> PathBuf {
    let root = Path::new(&project.path);
    match project.working_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(dir) => {
            let candidate = Path::new(dir);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                root.join(candidate)
            }
        }
        None => root.to_path_buf(),
    }
}

/// Build the child command. npm/yarn/pnpm are .cmd shims on Windows and the stored
/// string may carry quoting, so those go through the shell instead of argv.
fn build_test_command(project: &TestProject, extra_args: Option<&str>) -> Result<Command, String> {
    let command = match extra_args {
        Some(extra) if !extra.trim().is_empty() => {
            format!("{} {}", project.test_command, extra.trim())
        }
        _ => project.test_command.clone(),
    };
    let mut cmd = match command.split_whitespace().next().unwrap_or("") {
        "npm" | "yarn" | "pnpm" => {
            #[cfg(target_os = "windows")]
            {
                let mut c = Command::new("cmd");
                c.args(["/C", command.as_str()]);
                c
            }
            #[cfg(not(target_os = "windows"))]
            {
                let mut c = Command::new("sh");
                c.args(["-c", &command]);
                c
            }
        }
        "" => return Err("测试命令为空".to_string()),
        bin => {
            let mut c = Command::new(bin);
            c.args(command.split_whitespace().skip(1));
            c
        }
    };
    let work_dir = work_dir_of(&project);
    if !work_dir.is_dir() {
        return Err(format!("工作目录不存在：{}", work_dir.display()));
    }
    cmd.current_dir(work_dir);
    if let Some(ref env) = project.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

/// Kill the whole tree — the runner spawns node/cargo, which spawns its own workers.
fn kill_process_tree(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(target_os = "windows"))]
    let _ = pid;
}

/// One live-output event for the running suite.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOutputChunk {
    pub project_id: String,
    pub text: String,
    pub done: bool,
}

type ByteSink = Arc<Mutex<Vec<u8>>>;

/// Drain one pipe on its own thread: keep the full text for the final result and
/// append to the live tail as lines arrive, so the UI can follow along.
fn spawn_pipe_reader(
    pipe: Option<Box<dyn Read + Send>>,
    acc: ByteSink,
    live: Arc<Mutex<String>>,
    readers_left: Arc<AtomicUsize>,
) {
    let Some(pipe) = pipe else {
        readers_left.fetch_sub(1, Ordering::AcqRel);
        return;
    };
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(pipe);
        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if let Ok(mut buf) = acc.lock() {
                buf.extend_from_slice(&line);
            }
            if let Ok(mut tail) = live.lock() {
                tail.push_str(String::from_utf8_lossy(&line).trim_end_matches('\r'));
                tail.push('\n');
            }
        }
        // Only when *both* readers are done may the caller take the buffers: one
        // shared bool would let the first EOF release a half-filled log.
        readers_left.fetch_sub(1, Ordering::AcqRel);
    });
}

fn take_buffer(acc: &ByteSink) -> Vec<u8> {
    acc.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
}

fn flush_live_tail(app: Option<&AppHandle>, project_id: &str, live: &Arc<Mutex<String>>, done: bool) {
    let Some(app) = app else { return };
    let text = live
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    if text.is_empty() && !done {
        return;
    }
    let _ = app.emit(
        "test-run-output",
        TestOutputChunk {
            project_id: project_id.to_string(),
            text,
            done,
        },
    );
}

pub(crate) fn run_test_sync(
    conn: Arc<Mutex<rusqlite::Connection>>,
    project: TestProject,
    project_id: String,
    args: Option<String>,
    timeout: Duration,
    app: Option<AppHandle>,
) -> Result<TestRunResult, String> {
    // The cancellation token is keyed by project id: one live run per project.
    let guard_id = project_id.clone();
    let _guard = crate::cancellation::CancelGuard::new(&guard_id);

    let mut cmd = build_test_command(&project, args.as_deref())?;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动测试命令失败: {}", e))?;
    let pid = child.id();

    let started_at = chrono::Utc::now().to_rfc3339();
    let timer = Instant::now();

    // Drain both pipes on their own threads (a full 64KB buffer would otherwise stall
    // the runner before it can exit) and mirror the lines into a live tail the UI polls.
    let stdout_acc: ByteSink = Arc::new(Mutex::new(Vec::new()));
    let stderr_acc: ByteSink = Arc::new(Mutex::new(Vec::new()));
    let live: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let readers_left = Arc::new(AtomicUsize::new(2));
    spawn_pipe_reader(
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        Arc::clone(&stdout_acc),
        Arc::clone(&live),
        Arc::clone(&readers_left),
    );
    spawn_pipe_reader(
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        Arc::clone(&stderr_acc),
        Arc::clone(&live),
        readers_left,
    );

    let deadline = timer + timeout;
    // "" | "cancelled" | "timeout"
    let mut aborted = String::new();
    let exit_ok = loop {
        match child.try_wait().map_err(|e| format!("等待测试进程失败: {}", e))? {
            Some(status) => break status.success(),
            None => {
                if crate::cancellation::is_cancelled(&project_id) {
                    aborted = "cancelled".to_string();
                    break false;
                }
                if Instant::now() >= deadline {
                    aborted = "timeout".to_string();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
                flush_live_tail(app.as_ref(), &project_id, &live, false);
            }
        }
    };
    if !aborted.is_empty() {
        kill_process_tree(pid);
        let _ = child.kill();
    }
    let _ = child.wait();

    // Bounded wait for the readers to hit EOF: a grandchild can hold a pipe open
    // forever, and the result must not depend on that.
    let drain_deadline = Instant::now() + Duration::from_secs(3);
    while readers_left.load(Ordering::Acquire) > 0 && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stdout_buf = take_buffer(&stdout_acc);
    let stderr_buf = take_buffer(&stderr_acc);
    flush_live_tail(app.as_ref(), &project_id, &live, true);

    let completed_at = chrono::Utc::now().to_rfc3339();
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
    let mut full_output = format!("{}\n{}", stdout, stderr);
    if !aborted.is_empty() {
        full_output.push_str(&if aborted == "cancelled" {
            "\n[已取消] 测试进程树已被终止".to_string()
        } else {
            format!(
                "\n[超时] 运行超过 {} 分钟，测试进程树已被终止",
                (timeout.as_secs() + 59) / 60
            )
        });
    }

    // Parse test output
    let (mut status, mut total, mut passed, mut failed, mut skipped) =
        parse_test_output(&full_output, &project.framework);
    if !aborted.is_empty() {
        status = aborted.clone();
    } else if !exit_ok && status == "success" {
        // Non-zero exit with no failures counted: the runner itself broke.
        status = "failed".to_string();
    } else if exit_ok && total == 0 {
        status = "error".to_string();
    }

    // A machine-readable report beats scraping: exact counts plus per-case detail.
    let suites = load_structured_suites(&work_dir_of(&project), &stdout, &stderr);
    if let Some(ref suites) = suites {
        let (t, p, f, s) = suite_counts(suites);
        (total, passed, failed, skipped) = (t, p, f, s);
        if aborted.is_empty() {
            status = if failed > 0 {
                "failed"
            } else if total == 0 {
                "error"
            } else {
                "success"
            }
            .to_string();
        }
    }
    let suites_json = serde_json::to_string(&suites.clone().unwrap_or_default())
        .unwrap_or_else(|_| "[]".to_string());

    let duration_ms = timer.elapsed().as_millis() as u64;
    let test_id = format!("run-{}", chrono::Utc::now().timestamp_millis());
    let mut output_for_ui = truncate_tail(&full_output, MAX_STORED_OUTPUT_BYTES);

    // Bookkeeping must never lose a finished run: if the write fails (the project was
    // deleted mid-run, the DB is busy) the result still reaches the UI, with a warning.
    if let Err(e) = record_run(
        &conn,
        &test_id,
        &project_id,
        &started_at,
        &completed_at,
        duration_ms,
        &status,
        total,
        passed,
        failed,
        skipped,
        &output_for_ui,
        &suites_json,
    ) {
        full_output.push_str(&format!("\n[警告] 运行结果入库失败：{}", e));
        output_for_ui = truncate_tail(&full_output, MAX_STORED_OUTPUT_BYTES);
    }

    Ok(TestRunResult {
        project_id,
        id: test_id,
        started_at,
        completed_at,
        duration_ms,
        status,
        total_tests: total,
        passed,
        failed,
        skipped,
        output: output_for_ui,
        suites: suites.unwrap_or_default(),
    })
}

/// Persist one finished run: the detail row, the history row, the project's last-run
/// columns, and the per-project retention trim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_run(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    run_id: &str,
    project_id: &str,
    started_at: &str,
    completed_at: &str,
    duration_ms: u64,
    status: &str,
    total: u32,
    passed: u32,
    failed: u32,
    skipped: u32,
    output: &str,
    suites_json: &str,
) -> Result<(), String> {
    let conn = conn.lock().map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO test_runs (id, project_id, started_at, completed_at, duration_ms, status, total_tests, passed, failed, skipped, output, suites) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            run_id,
            project_id,
            started_at,
            completed_at,
            duration_ms,
            status,
            total,
            passed,
            failed,
            skipped,
            output,
            suites_json,
        ]
    ).map_err(|e| format!("Failed to save test run: {}", e))?;

    conn.execute(
        "INSERT INTO test_history (project_id, run_id, timestamp, status, total, passed, failed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![project_id, run_id, completed_at, status, total, passed, failed]
    ).map_err(|e| format!("Failed to save test history: {}", e))?;

    conn.execute(
        "UPDATE test_projects SET last_run_at = ?1, last_status = ?2, updated_at = ?3 WHERE id = ?4",
        rusqlite::params![completed_at, status, completed_at, project_id]
    ).map_err(|e| format!("Failed to update project: {}", e))?;

    prune_run_history(&conn, project_id, RUN_RETENTION_PER_PROJECT)
        .map_err(|e| format!("Failed to prune test runs: {}", e))?;
    prune_history(&conn, project_id, HISTORY_RETENTION_PER_PROJECT)
        .map_err(|e| format!("Failed to prune test history: {}", e))?;

    Ok(())
}

/// Keep only the newest `keep` run rows of a project; each row can carry 64KB of log,
/// so an unbounded table would grow the database without limit.
pub(crate) fn prune_run_history(conn: &rusqlite::Connection, project_id: &str, keep: i64) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM test_runs WHERE project_id = ?1 AND id NOT IN (
             SELECT id FROM test_runs WHERE project_id = ?1
             ORDER BY completed_at DESC, rowid DESC LIMIT ?2
         )",
        rusqlite::params![project_id, keep],
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn prune_history(conn: &rusqlite::Connection, project_id: &str, keep: i64) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM test_history WHERE project_id = ?1 AND id NOT IN (
             SELECT id FROM test_history WHERE project_id = ?1 ORDER BY id DESC LIMIT ?2
         )",
        rusqlite::params![project_id, keep],
    )
    .map_err(|e| e.to_string())
}

/// Read one stored run back, so a history row can show its full result again.
pub(crate) fn run_by_id_sync(conn: &rusqlite::Connection, run_id: &str) -> Result<TestRunResult, String> {
    conn.query_row(
        "SELECT id, project_id, started_at, completed_at, duration_ms, status, total_tests, passed, failed, skipped, output, suites FROM test_runs WHERE id = ?1",
        [run_id],
        |row| {
            Ok(TestRunResult {
                id: row.get(0)?,
                project_id: row.get(1)?,
                started_at: row.get(2)?,
                completed_at: row.get(3)?,
                duration_ms: row.get(4)?,
                status: row.get(5)?,
                total_tests: row.get(6)?,
                passed: row.get(7)?,
                failed: row.get(8)?,
                skipped: row.get(9)?,
                output: row.get(10)?,
                suites: serde_json::from_str(&row.get::<_, String>(11)?)
                    .unwrap_or_default(),
            })
        },
    )
    .map_err(|e| format!("未找到该次运行记录: {}", e))
}

#[tauri::command]
pub fn get_test_run(state: State<DbState>, run_id: String) -> Result<TestRunResult, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    run_by_id_sync(&conn, &run_id)
}

/// Get test history for a project
#[tauri::command]
pub fn get_test_history(state: State<DbState>, project_id: Option<String>) -> Result<Vec<TestHistoryEntry>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;

    let sql = if project_id.is_some() {
        "SELECT id, project_id, run_id, timestamp, status, total, passed, failed FROM test_history WHERE project_id = ? ORDER BY timestamp DESC LIMIT 50"
    } else {
        "SELECT id, project_id, run_id, timestamp, status, total, passed, failed FROM test_history ORDER BY timestamp DESC LIMIT 50"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| format!("Failed to prepare query: {}", e))?;

    let entries = if let Some(pid) = project_id {
        stmt.query_map([&pid], |row| {
            Ok(TestHistoryEntry {
                id: row.get(0)?,
                project_id: row.get(1)?,
                run_id: row.get(2)?,
                timestamp: row.get(3)?,
                status: row.get(4)?,
                total: row.get(5)?,
                passed: row.get(6)?,
                failed: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query history: {}", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse history: {}", e))?
    } else {
        stmt.query_map([], |row| {
            Ok(TestHistoryEntry {
                id: row.get(0)?,
                project_id: row.get(1)?,
                run_id: row.get(2)?,
                timestamp: row.get(3)?,
                status: row.get(4)?,
                total: row.get(5)?,
                passed: row.get(6)?,
                failed: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query history: {}", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse history: {}", e))?
    };

    Ok(entries)
}

/// Load the default model row into the shared AI request config.
/// The column order must match the closure below (it mirrors `ai_commands::AIModelConfig`).
fn load_default_model_config(
    conn: &rusqlite::Connection,
) -> Result<crate::ai_commands::AIModelConfig, String> {
    conn.query_row(
        "SELECT id, name, provider, api_key, auth_type, base_url, model, temperature, max_tokens, is_default, created_at, updated_at FROM ai_models WHERE is_default = 1 LIMIT 1",
        [],
        |row| {
            Ok(crate::ai_commands::AIModelConfig {
                id: row.get(0)?,
                name: row.get(1)?,
                provider: row.get(2)?,
                api_key: row.get(3)?,
                auth_type: row.get(4)?,
                base_url: row.get(5)?,
                model: row.get(6)?,
                temperature: row.get(7)?,
                max_tokens: row.get(8)?,
                is_default: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        },
    )
    .map_err(|e| format!("未能读取默认 AI 模型，请先在「模型配置」设置默认模型: {}", e))
}

/// Generate test code using AI
#[tauri::command]
pub async fn generate_test_code(
    state: State<'_, DbState>,
    source_code: String,
    file_path: String,
    framework: String,
    coverage_level: String,
    mock_strategy: String,
    assert_style: String,
) -> Result<String, String> {
    // Get default AI model from database
    let mut config = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        load_default_model_config(&conn)?
    };
    // Generation needs long, deterministic output; the request config is a local copy,
    // so this does not touch the user's saved model settings.
    config.max_tokens = 4096;
    config.temperature = 0.2;

    // Build prompt
    let prompt = format!(
        "请为以下代码生成单元测试。\n\n\
         代码:\n\
         ```\n\
         {}\n\
         ```\n\n\
         要求:\n\
         1. 测试框架: {}\n\
         2. 测试覆盖度: {} (全面测试包括正常情况、边界值、异常情况)\n\
         3. Mock 策略: {}\n\
         4. 断言风格: {}\n\
         5. 测试文件路径: {}\n\n\
         生成的测试代码应该:\n\
         - 使用 describe/it 结构清晰组织\n\
         - 每个测试用例有清晰的描述\n\
         - 包含必要的 setup 和 teardown\n\
         - 使用恰当的断言和匹配器\n\
         - 处理异步情况（如果需要）\n\
         - 注释关键测试意图\n\n\
         只返回测试代码，不要包含任何解释文字。",
        source_code, framework, coverage_level, mock_strategy, assert_style, file_path
    );

    // Call AI generate_text
    let req = crate::ai_commands::GenerateTextRequest {
        config,
        system: "你是测试代码生成专家，只返回代码，不要包含任何解释文字。".to_string(),
        user: prompt,
    };

    let result = crate::ai_commands::generate_text(req).await?;
    Ok(result)
}

/// Diagnose test failure using AI
#[tauri::command]
pub async fn diagnose_test_failure(
    state: State<'_, DbState>,
    test_code: String,
    source_code: String,
    error_message: String,
    test_name: String,
) -> Result<String, String> {
    // Get default AI model from database
    let mut config = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        load_default_model_config(&conn)?
    };
    config.max_tokens = 2048;
    config.temperature = 0.3;

    // Build prompt
    let prompt = format!(
        "请分析以下失败的测试用例，提供根因分析和修复建议。\n\n\
         测试名称: {}\n\n\
         错误信息:\n\
         {}\n\n\
         测试代码:\n\
         ```\n\
         {}\n\
         ```\n\n\
         被测代码:\n\
         ```\n\
         {}\n\
         ```\n\n\
         请按以下格式输出:\n\n\
         ## 根因分析\n\
         [分析失败的根本原因]\n\n\
         ## 预期行为\n\
         [描述测试期望的正确行为]\n\n\
         ## 实际行为\n\
         [描述实际发生的行为]\n\n\
         ## 修复建议\n\
         [提供具体的修复代码或修改建议]\n\n\
         ## 修复代码\n\
         ```\n\
         [修复后的代码片段，如果适用]\n\
         ```",
        test_name, error_message, test_code, source_code
    );

    // Call AI generate_text
    let req = crate::ai_commands::GenerateTextRequest {
        config,
        system: "你是测试失败诊断专家，按要求的章节结构输出分析。".to_string(),
        user: prompt,
    };

    let result = crate::ai_commands::generate_text(req).await?;
    Ok(result)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub lines: CoverageMetric,
    pub statements: CoverageMetric,
    pub branches: CoverageMetric,
    pub functions: CoverageMetric,
    pub files: Vec<CoverageFile>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMetric {
    pub total: u32,
    pub covered: u32,
    pub percentage: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFile {
    pub path: String,
    pub lines: CoverageMetric,
    pub statements: CoverageMetric,
    pub branches: CoverageMetric,
    pub functions: CoverageMetric,
}

/// Read coverage report for a project
#[tauri::command]
pub fn read_coverage_report(
    state: State<DbState>,
    project_id: String,
) -> Result<CoverageReport, String> {
    // Get project from database
    let projects = load_test_projects(state)?;
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project not found: {}", project_id))?
        .clone();

    // Only formats we can actually parse: pytest's binary `.coverage` database is
    // deliberately not a candidate — reading it as JSON always failed.
    let (candidates, hint): (&[&str], &str) = match project.framework.as_str() {
        "jest" | "vitest" => (
            &["coverage/coverage-final.json", "coverage/coverage-summary.json"],
            "请先运行带覆盖率的测试，例如 npm test -- --coverage",
        ),
        "cargo" => (
            &["target/llvm-cov/coverage.json"],
            "请先运行 cargo llvm-cov --json --output-path target/llvm-cov/coverage.json",
        ),
        "pytest" => (
            &["coverage.json", "coverage/coverage.json"],
            "请先运行 pytest --cov --cov-report=json",
        ),
        other => return Err(format!("暂不支持 {} 框架的覆盖率报告", other)),
    };
    let root = Path::new(&project.path);
    let found = candidates
        .iter()
        .map(|p| root.join(p))
        .find(|p| p.exists())
        .ok_or_else(|| format!("未找到覆盖率报告（{}），{}", candidates.join(" / "), hint))?;

    let content = fs::read_to_string(&found)
        .map_err(|e| format!("读取覆盖率报告失败: {}", e))?;

    // The file name says which writer produced it, which is sturdier than trusting
    // the framework label the user typed when adding the project.
    let report = match found.file_name().and_then(|n| n.to_str()).unwrap_or("") {
        "coverage-final.json" => parse_istanbul_coverage(&content)?,
        "coverage-summary.json" => parse_istanbul_summary_coverage(&content)?,
        _ => match project.framework.as_str() {
            "cargo" => parse_cargo_coverage(&content)?,
            "pytest" => parse_pytest_coverage(&content)?,
            _ => parse_istanbul_coverage(&content)?,
        },
    };

    Ok(report)
}

/// Istanbul `coverage-summary.json`: one `{ total, covered, pct }` block per
/// category, for "total" and for each measured file.
fn summary_metric(parent: Option<&serde_json::Value>, key: &str) -> CoverageMetric {
    let node = parent.and_then(|p| p.get(key));
    let total = node
        .and_then(|n| n.get("total"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let covered = node
        .and_then(|n| n.get("covered"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let percentage = node
        .and_then(|n| n.get("pct"))
        .and_then(|v| v.as_f64())
        .unwrap_or(if total > 0 {
            covered as f64 * 100.0 / total as f64
        } else {
            0.0
        });
    CoverageMetric {
        total,
        covered,
        percentage,
    }
}

pub(crate) fn parse_istanbul_summary_coverage(content: &str) -> Result<CoverageReport, String> {
    let json: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("解析覆盖率 JSON 失败: {}", e))?;
    let obj = json
        .as_object()
        .ok_or_else(|| "覆盖率 JSON 结构不符合预期".to_string())?;

    let mut files = Vec::new();
    for (path, entry) in obj {
        // "total" is the roll-up; newer writers also emit a "root" path entry.
        if path == "total" || path == "root" || entry.get("lines").is_none() {
            continue;
        }
        files.push(CoverageFile {
            path: path.clone(),
            lines: summary_metric(Some(entry), "lines"),
            statements: summary_metric(Some(entry), "statements"),
            branches: summary_metric(Some(entry), "branches"),
            functions: summary_metric(Some(entry), "functions"),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let total = obj.get("total");
    Ok(CoverageReport {
        lines: summary_metric(total, "lines"),
        statements: summary_metric(total, "statements"),
        branches: summary_metric(total, "branches"),
        functions: summary_metric(total, "functions"),
        files,
    })
}

/// Parse Istanbul/Jest/Vitest coverage format
pub(crate) fn parse_istanbul_coverage(content: &str) -> Result<CoverageReport, String> {
    let json: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse coverage JSON: {}", e))?;

    let mut files = Vec::new();
    let mut total_lines = 0u32;
    let mut covered_lines = 0u32;
    let mut total_statements = 0u32;
    let mut covered_statements = 0u32;
    let mut total_branches = 0u32;
    let mut covered_branches = 0u32;
    let mut total_functions = 0u32;
    let mut covered_functions = 0u32;

    // Istanbul format: { "path": { "statementMap": {}, "fnMap": {}, "branchMap": {}, "s": {}, "f": {}, "b": {} } }
    if let Some(obj) = json.as_object() {
        for (path, file_data) in obj {
            let file_path = path.clone();

            // Parse statements
            let (stmt_total, stmt_covered) = parse_istanbul_metric(file_data.get("s"), file_data.get("statementMap"));

            // Parse functions
            let (fn_total, fn_covered) = parse_istanbul_metric(file_data.get("f"), file_data.get("fnMap"));

            // Parse branches
            let (br_total, br_covered) = parse_istanbul_branch_metric(file_data.get("b"), file_data.get("branchMap"));

            // For lines, we use statements as approximation
            let lines = CoverageMetric {
                total: stmt_total,
                covered: stmt_covered,
                percentage: if stmt_total > 0 {
                    (stmt_covered as f64 / stmt_total as f64) * 100.0
                } else {
                    0.0
                },
            };

            let statements = CoverageMetric {
                total: stmt_total,
                covered: stmt_covered,
                percentage: if stmt_total > 0 {
                    (stmt_covered as f64 / stmt_total as f64) * 100.0
                } else {
                    0.0
                },
            };

            let branches = CoverageMetric {
                total: br_total,
                covered: br_covered,
                percentage: if br_total > 0 {
                    (br_covered as f64 / br_total as f64) * 100.0
                } else {
                    0.0
                },
            };

            let functions = CoverageMetric {
                total: fn_total,
                covered: fn_covered,
                percentage: if fn_total > 0 {
                    (fn_covered as f64 / fn_total as f64) * 100.0
                } else {
                    0.0
                },
            };

            files.push(CoverageFile {
                path: file_path,
                lines,
                statements,
                branches,
                functions,
            });

            total_lines += stmt_total;
            covered_lines += stmt_covered;
            total_statements += stmt_total;
            covered_statements += stmt_covered;
            total_branches += br_total;
            covered_branches += br_covered;
            total_functions += fn_total;
            covered_functions += fn_covered;
        }
    }

    Ok(CoverageReport {
        lines: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        statements: CoverageMetric {
            total: total_statements,
            covered: covered_statements,
            percentage: if total_statements > 0 {
                (covered_statements as f64 / total_statements as f64) * 100.0
            } else {
                0.0
            },
        },
        branches: CoverageMetric {
            total: total_branches,
            covered: covered_branches,
            percentage: if total_branches > 0 {
                (covered_branches as f64 / total_branches as f64) * 100.0
            } else {
                0.0
            },
        },
        functions: CoverageMetric {
            total: total_functions,
            covered: covered_functions,
            percentage: if total_functions > 0 {
                (covered_functions as f64 / total_functions as f64) * 100.0
            } else {
                0.0
            },
        },
        files,
    })
}

fn parse_istanbul_metric(
    values: Option<&serde_json::Value>,
    _map: Option<&serde_json::Value>,
) -> (u32, u32) {
    if let Some(obj) = values.and_then(|v| v.as_object()) {
        let total = obj.len() as u32;
        let covered = obj.values().filter(|v| v.as_u64().unwrap_or(0) > 0).count() as u32;
        (total, covered)
    } else {
        (0, 0)
    }
}

fn parse_istanbul_branch_metric(
    values: Option<&serde_json::Value>,
    _map: Option<&serde_json::Value>,
) -> (u32, u32) {
    if let Some(obj) = values.and_then(|v| v.as_object()) {
        let mut total = 0u32;
        let mut covered = 0u32;
        for (_, branches) in obj {
            if let Some(arr) = branches.as_array() {
                total += arr.len() as u32;
                // Istanbul writes plain hit counts ("b": {"0": [3, 0]}); reading them
                // as nested arrays made every branch look uncovered.
                covered += arr
                    .iter()
                    .filter(|v| match v {
                        serde_json::Value::Array(inner) => inner
                            .first()
                            .and_then(|n| n.as_u64())
                            .unwrap_or(0)
                            > 0,
                        other => other.as_u64().unwrap_or(0) > 0,
                    })
                    .count() as u32;
            }
        }
        (total, covered)
    } else {
        (0, 0)
    }
}

/// Parse Cargo/llvm-cov coverage format
pub(crate) fn parse_cargo_coverage(content: &str) -> Result<CoverageReport, String> {
    let json: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse coverage JSON: {}", e))?;

    let mut files = Vec::new();
    let mut total_lines = 0u32;
    let mut covered_lines = 0u32;

    // llvm-cov format: { "data": [ { "files": [ { "filename": "...", "summary": { "lines": { "count": N, "covered": M } } } ] } ] }
    if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        for entry in data {
            if let Some(file_list) = entry.get("files").and_then(|f| f.as_array()) {
                for file in file_list {
                    let path = file.get("filename").and_then(|f| f.as_str()).unwrap_or("").to_string();
                    let summary = file.get("summary");

                    let lines = summary
                        .and_then(|s| s.get("lines"))
                        .map(|l| {
                            let total = l.get("count").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
                            let covered = l.get("covered").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
                            CoverageMetric {
                                total,
                                covered,
                                percentage: if total > 0 {
                                    (covered as f64 / total as f64) * 100.0
                                } else {
                                    0.0
                                },
                            }
                        })
                        .unwrap_or(CoverageMetric { total: 0, covered: 0, percentage: 0.0 });

                    total_lines += lines.total;
                    covered_lines += lines.covered;

                    files.push(CoverageFile {
                        path,
                        lines: lines.clone(),
                        statements: lines.clone(),
                        branches: lines.clone(),
                        functions: lines,
                    });
                }
            }
        }
    }

    Ok(CoverageReport {
        lines: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        statements: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        branches: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        functions: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        files,
    })
}

/// Cancel the live run of a project (the run kills its own process tree).
#[tauri::command]
pub fn cancel_test_run(project_id: String) -> Result<(), String> {
    if !crate::cancellation::is_active(&project_id) {
        return Err("该项目没有正在运行的测试".to_string());
    }
    crate::cancellation::cancel_request(&project_id);
    Ok(())
}

/// Parse pytest-cov coverage format
pub(crate) fn parse_pytest_coverage(content: &str) -> Result<CoverageReport, String> {
    let json: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse coverage JSON: {}", e))?;

    let mut files = Vec::new();
    let mut total_lines = 0u32;
    let mut covered_lines = 0u32;

    // pytest-cov format: { "files": { "path": { "summary": { "covered_lines": N, "num_statements": M } } } }
    if let Some(file_map) = json.get("files").and_then(|f| f.as_object()) {
        for (path, file_data) in file_map {
            let summary = file_data.get("summary");
            let total = summary
                .and_then(|s| s.get("num_statements"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let covered = summary
                .and_then(|s| s.get("covered_lines"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            let lines = CoverageMetric {
                total,
                covered,
                percentage: if total > 0 {
                    (covered as f64 / total as f64) * 100.0
                } else {
                    0.0
                },
            };

            total_lines += total;
            covered_lines += covered;

            files.push(CoverageFile {
                path: path.clone(),
                lines: lines.clone(),
                statements: lines.clone(),
                branches: lines.clone(),
                functions: lines,
            });
        }
    }

    Ok(CoverageReport {
        lines: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        statements: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        branches: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        functions: CoverageMetric {
            total: total_lines,
            covered: covered_lines,
            percentage: if total_lines > 0 {
                (covered_lines as f64 / total_lines as f64) * 100.0
            } else {
                0.0
            },
        },
        files,
    })
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn vitest_summary_is_not_double_counted_with_verbose_marks() {
        let output = "✓ src/a.test.ts (3 tests)\n  ✓ works\n  ✓ also works\n  ✓ another\n\
                      Tests  5 passed | 1 failed (6)";
        let (status, total, passed, failed, skipped) = parse_test_output(output, "vitest");
        assert_eq!((status.as_str(), total, passed, failed, skipped), ("failed", 6, 5, 1, 0));
    }

    #[test]
    fn jest_marks_are_counted_only_when_no_summary_is_present() {
        let output = "✓ a\n✓ b\n✗ c\n○ d";
        let (_, total, passed, failed, skipped) = parse_test_output(output, "jest");
        assert_eq!((total, passed, failed, skipped), (4, 2, 1, 1));
    }

    #[test]
    fn output_that_merely_mentions_skip_does_not_inflate_skipped() {
        // The old branch added one skip per line whenever "skip" appeared anywhere.
        let output = "Tests  2 passed (2)\nsome noise\nanother line\nyet another";
        let (_, total, passed, failed, skipped) = parse_test_output(output, "vitest");
        assert_eq!((total, passed, failed, skipped), (2, 2, 0, 0));
    }

    #[test]
    fn pytest_reads_its_single_summary_line() {
        let output = "test_a PASSED\ntest_b PASSED\ntest_c PASSED\n\
                      ========= 3 passed, 1 failed, 2 skipped, 1 error in 0.42s =========";
        let (status, total, passed, failed, skipped) = parse_test_output(output, "pytest");
        assert_eq!(status, "failed");
        assert_eq!((total, passed, failed, skipped), (3 + 2 + 2, 3, 2, 2));
    }

    #[test]
    fn cargo_sums_test_result_lines_and_running_headers() {
        let output = "running 12 tests\ntest result: ok. 10 passed; 0 failed; 2 ignored; 0 measured\n";
        let (_, total, passed, failed, skipped) = parse_test_output(output, "cargo");
        assert_eq!((total, passed, failed, skipped), (12, 10, 0, 2));
    }

    #[test]
    fn gotest_counts_per_test_markers_only() {
        let output = "--- PASS: TestA (0.00s)\n--- PASS: TestB (0.00s)\n--- FAIL: TestC (0.00s)\nPASS\nok  \tpkg 0.1s";
        let (_, total, passed, failed, skipped) = parse_test_output(output, "gotest");
        assert_eq!((total, passed, failed, skipped), (3, 2, 1, 0));
    }

    #[test]
    fn clean_run_reports_success_and_empty_run_reports_error() {
        assert_eq!(parse_test_output("Tests  3 passed (3)", "vitest").0, "success");
        assert_eq!(parse_test_output("no tests here", "vitest").0, "error");
    }

    #[test]
    fn truncate_tail_keeps_the_end_and_never_splits_utf8() {
        assert_eq!(truncate_tail("short", 100), "short");
        let text = "失败信息".repeat(50); // 600 bytes
        let tail: String = text.chars().rev().take(20).collect::<Vec<_>>()
            .into_iter().rev().collect();
        let cut = truncate_tail(&text, 60);
        assert!(cut.ends_with(&tail));
        assert!(cut.starts_with("…（已省略前 "));
        assert!(cut.len() < text.len());
    }

    #[test]
    fn blank_columns_load_as_unset() {
        assert_eq!(opt_text(Some(String::new())), None);
        assert_eq!(opt_text(Some("  ".to_string())), None);
        assert_eq!(opt_text(Some("npm test".to_string())).as_deref(), Some("npm test"));
    }
}
