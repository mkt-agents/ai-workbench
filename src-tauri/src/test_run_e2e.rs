//! End-to-end checks for the test-assistant commands: they spawn real child
//! processes, hit a real SQLite schema and exercise the timeout/cancel paths.

#[cfg(test)]
mod tests {
    use crate::test_commands::*;
    use crate::test_output_parsers::{parse_jest_style_results, suite_counts, TestCase};
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};
    use std::process::{Command as OsCommand, Stdio};
    use std::time::Duration;

    fn node_available() -> bool {
        OsCommand::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Fresh scratch directory — leftovers from an earlier run would pass the
    /// "was the process really killed" assertions by accident.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiwb-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> std::sync::Arc<std::sync::Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        std::sync::Arc::new(std::sync::Mutex::new(conn))
    }

    fn project(dir: &Path, id: &str, command: &str, framework: &str) -> TestProject {
        TestProject {
            id: id.to_string(),
            name: id.to_string(),
            path: dir.to_string_lossy().to_string(),
            project_type: "frontend".to_string(),
            framework: framework.to_string(),
            test_command: command.to_string(),
            args: None,
            working_dir: None,
            env: None,
            enabled: true,
            created_at: "2026-09-20T00:00:00Z".to_string(),
            updated_at: "2026-09-20T00:00:00Z".to_string(),
            last_run_at: None,
            last_status: None,
        }
    }

    fn write_script(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn run(
        conn: &std::sync::Arc<std::sync::Mutex<Connection>>,
        dir: &Path,
        id: &str,
        args: Option<String>,
        timeout: Duration,
    ) -> Result<TestRunResult, String> {
        let project = project(dir, id, "node t.js", "vitest");
        add_project_sync(&conn.lock().unwrap(), project.clone()).ok();
        run_test_sync(
            std::sync::Arc::clone(conn),
            project,
            id.to_string(),
            args,
            timeout,
            None,        )
    }

    #[test]
    fn a_failed_write_still_returns_the_parsed_result() {
        if !node_available() {
            return;
        }
        let dir = scratch("orphan");
        write_script(&dir, "t.js", "console.log('Tests  3 passed (3)');");
        let conn = db();
        let project = project(&dir, "p-orphan", "node t.js", "vitest");

        // No test_projects row exists, so the bookkeeping INSERT fails on the FK —
        // the run itself must still deliver its numbers.
        let result = run_test_sync(
            std::sync::Arc::clone(&conn),
            project,
            "p-orphan".to_string(),
            None,
            Duration::from_secs(60),
            None,        )
        .unwrap();

        assert_eq!(result.total_tests, 3);
        assert_eq!(result.status, "success");
        assert!(
            result.output.contains("入库失败"),
            "output: {}",
            result.output
        );
        let rows: i64 = conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM test_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passing_suite_is_parsed_and_recorded() {
        if !node_available() {
            eprintln!("skipping: node not on PATH");
            return;
        }
        let dir = scratch("pass");
        write_script(&dir, "t.js", "console.log('Tests  3 passed (3)');");
        let conn = db();
        add_project_sync(&conn.lock().unwrap(), project(&dir, "p-pass", "node t.js", "vitest"))
            .unwrap();

        let result = run(&conn, &dir, "p-pass", None, Duration::from_secs(60)).unwrap();

        assert_eq!(result.status, "success");
        assert_eq!((result.total_tests, result.passed, result.failed), (3, 3, 0));
        assert!(result.duration_ms > 0);
        assert!(result.output.contains("3 passed"), "output: {}", result.output);

        let guard = conn.lock().unwrap();
        let (run_status, run_total): (String, u32) = guard
            .query_row("SELECT status, total_tests FROM test_runs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((run_status.as_str(), run_total), ("success", 3));
        let history: i64 = guard
            .query_row("SELECT COUNT(*) FROM test_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(history, 1);
        let (last_status, last_run_at): (String, String) = guard
            .query_row(
                "SELECT last_status, last_run_at FROM test_projects WHERE id = 'p-pass'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_status, "success");
        assert!(!last_run_at.is_empty());
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_without_a_bad_exit_code_still_report_failed() {
        if !node_available() {
            return;
        }
        let dir = scratch("fail");
        write_script(&dir, "t.js", "console.log('Tests  2 passed | 1 failed (3)');");
        let conn = db();

        let result = run(&conn, &dir, "p-fail", None, Duration::from_secs(60)).unwrap();

        assert_eq!(result.status, "failed");
        assert_eq!((result.passed, result.failed), (2, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_with_no_tests_is_not_reported_as_success() {
        if !node_available() {
            return;
        }
        let dir = scratch("empty");
        write_script(&dir, "t.js", "console.log('no tests matched');process.exit(1);");
        let conn = db();

        let result = run(&conn, &dir, "p-empty", None, Duration::from_secs(60)).unwrap();

        // A suite that ran but matched nothing is a broken invocation, not a pass.
        assert_eq!(result.status, "error");
        assert_eq!(result.total_tests, 0);
        assert!(
            result.output.contains("no tests matched"),
            "output: {}",
            result.output
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_working_directory_fails_fast() {
        let dir = scratch("badworkdir");
        let conn = db();
        let mut p = project(&dir, "p-wd", "node t.js", "vitest");
        p.working_dir = Some("packages/nope".to_string());

        let err = run_test_sync(
            std::sync::Arc::clone(&conn),
            p,
            "p-wd".to_string(),
            None,
            Duration::from_secs(5),
            None,        )
        .unwrap_err();

        assert!(err.contains("工作目录不存在"), "got: {}", err);
        assert!(err.contains("packages"), "got: {}", err);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_command_is_reported_not_hung() {
        let dir = scratch("spawnerr");
        let conn = db();
        let err = run_test_sync(
            std::sync::Arc::clone(&conn),
            project(&dir, "p-missing", "definitely-not-a-real-binary test", "custom"),
            "p-missing".to_string(),
            None,
            Duration::from_secs(5),
            None,        )
        .unwrap_err();
        assert!(err.contains("启动测试命令失败"), "got: {}", err);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeout_kills_the_whole_process_tree() {
        if !node_available() {
            return;
        }
        let dir = scratch("timeout");
        // The parent hangs for 20s and spawns a grandchild that writes a marker after
        // 6s. Killing only the parent would still leave the marker behind.
        write_script(
            &dir,
            "t.js",
            concat!(
                "const { spawn } = require('child_process');\n",
                "const child = [process.execPath, '-e',",
                " \"setTimeout(() => require('fs').writeFileSync('grand.txt', 'x'), 6000)\"];\n",
                "spawn(child[0], child.slice(1), { stdio: 'ignore', cwd: process.cwd() });\n",
                "setTimeout(() => {}, 20000);\n"
            ),
        );
        let conn = db();
        let started = std::time::Instant::now();

        let result = run(&conn, &dir, "p-timeout", None, Duration::from_secs(2)).unwrap();

        assert_eq!(result.status, "timeout");
        assert!(result.output.contains("[超时]"), "output: {}", result.output);
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "took {:?}",
            started.elapsed()
        );

        std::thread::sleep(Duration::from_secs(8));
        assert!(
            !dir.join("grand.txt").exists(),
            "grandchild survived the timeout — the process tree was not killed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_request_stops_a_running_suite() {
        if !node_available() {
            return;
        }
        let dir = scratch("cancel");
        write_script(&dir, "t.js", "setTimeout(() => {}, 30000);\n");
        let conn = db();
        let thread_conn = std::sync::Arc::clone(&conn);
        let project_dir = dir.clone();
        add_project_sync(
            &conn.lock().unwrap(),
            project(&dir, "p-cancel", "node t.js", "vitest"),
        )
        .unwrap();

        let handle = std::thread::spawn(move || {
            run_test_sync(
                thread_conn,
                project(&project_dir, "p-cancel", "node t.js", "vitest"),
                "p-cancel".to_string(),
                None,
                Duration::from_secs(120),
                None,            )
        });

        while !crate::cancellation::is_active("p-cancel") {
            std::thread::sleep(Duration::from_millis(20));
        }
        crate::cancellation::cancel_request("p-cancel");

        let result = handle.join().unwrap().unwrap();
        assert_eq!(result.status, "cancelled");
        assert!(result.output.contains("[已取消]"), "output: {}", result.output);

        let status: String = conn
            .lock()
            .unwrap()
            .query_row("SELECT status FROM test_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "cancelled");

        // The token must be cleared so a later run of the same project is not
        // silently pre-cancelled.
        assert!(!crate::cancellation::is_cancelled("p-cancel"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_args_reach_the_test_command() {
        if !node_available() {
            return;
        }
        let dir = scratch("args");
        write_script(
            &dir,
            "t.js",
            "console.log('Tests  1 passed (1) ' + process.argv.slice(2).join(' '));\n",
        );
        let conn = db();

        let result = run(
            &conn,
            &dir,
            "p-args",
            Some("--coverage".to_string()),
            Duration::from_secs(60),
        )
        .unwrap();

        assert!(result.output.contains("--coverage"), "output: {}", result.output);
        assert_eq!(result.status, "success");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn working_dir_and_env_are_used_for_the_run() {
        if !node_available() {
            return;
        }
        let dir = scratch("workdir");
        std::fs::create_dir_all(dir.join("packages/web")).unwrap();
        write_script(
            &dir,
            "t.js",
            "console.log('cwd=' + process.cwd().replace(/\\\\/g,'/'));\n\
             console.log('CI=' + process.env.CI);\n\
             console.log('Tests  1 passed (1)');\n",
        );
        let conn = db();
        let mut p = project(
            &dir,
            "p-workdir",
            "node ../../t.js",
            "vitest",
        );
        p.working_dir = Some("packages/web".to_string());
        p.env = Some(std::collections::HashMap::from([(
            "CI".to_string(),
            "1".to_string(),
        )]));
        add_project_sync(&conn.lock().unwrap(), p.clone()).unwrap();

        let result = run_test_sync(
            std::sync::Arc::clone(&conn),
            p,
            "p-workdir".to_string(),
            None,
            Duration::from_secs(60),
            None,        )
        .unwrap();

        assert!(
            result.output.contains("cwd=") && result.output.contains("packages/web"),
            "output: {}",
            result.output
        );
        assert!(result.output.contains("CI=1"), "output: {}", result.output);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_crud_roundtrip_keeps_optional_fields() {
        let dir = scratch("crud");
        let conn = db();
        let mut p = project(&dir, "p-1", "npm test", "jest");
        p.env = Some(std::collections::HashMap::from([(
            "CI".to_string(),
            "true".to_string(),
        )]));
        add_project_sync(&conn.lock().unwrap(), p).unwrap();

        let loaded = load_projects_sync(&conn.lock().unwrap()).unwrap();
        assert_eq!(loaded.len(), 1);
        // Blank columns come back as "unset", not as empty strings.
        assert_eq!(loaded[0].args, None);
        assert_eq!(loaded[0].working_dir, None);
        assert_eq!(
            loaded[0].env.as_ref().unwrap().get("CI").map(String::as_str),
            Some("true")
        );

        update_project_sync(
            &conn.lock().unwrap(),
            "p-1".to_string(),
            serde_json::json!({
                "workingDir": "packages/web",
                "type": "backend",
                "args": "--run",
                "enabled": false,
                "env": { "NODE_ENV": "test" }
            }),
        )
        .unwrap();

        let loaded = load_projects_sync(&conn.lock().unwrap()).unwrap();
        assert_eq!(loaded[0].working_dir.as_deref(), Some("packages/web"));
        assert_eq!(loaded[0].project_type, "backend");
        assert_eq!(loaded[0].args.as_deref(), Some("--run"));
        assert!(!loaded[0].enabled);
        assert_eq!(
            loaded[0].env.as_ref().unwrap().get("NODE_ENV").map(String::as_str),
            Some("test")
        );

        delete_project_sync(&conn.lock().unwrap(), "p-1").unwrap();
        assert!(load_projects_sync(&conn.lock().unwrap()).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detection_reads_package_json() {
        let dir = scratch("detect");
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test":"vitest run"},"devDependencies":{"vitest":"^2.0.0"}}"#,
        )
        .unwrap();

        let result = detect_project_type(dir.to_string_lossy().to_string()).unwrap();
        assert!(result.detected);
        assert_eq!(result.project_type.as_deref(), Some("frontend"));
        assert_eq!(result.framework.as_deref(), Some("vitest"));
        assert_eq!(result.reason, "package.json + test script");
        let command = result.test_command.expect("a test command was suggested");
        assert!(command.contains("test"), "got: {}", command);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detection_of_a_rust_project() {
        let dir = scratch("detect-rs");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        let result = detect_project_type(dir.to_string_lossy().to_string()).unwrap();
        assert!(result.detected);
        assert_eq!(result.framework.as_deref(), Some("cargo"));
        assert_eq!(result.reason, "Cargo.toml");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_directory_is_reported_as_undetected() {
        let dir = scratch("detect-none");
        let result = detect_project_type(dir.to_string_lossy().to_string()).unwrap();
        assert!(!result.detected);
        assert_eq!(result.reason, "none");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_parsers_read_real_report_shapes() {
        // Istanbul coverage-final.json (jest / vitest default writer)
        let final_json = r#"{
            "/abs/src/a.ts": {"path":"/abs/src/a.ts",
              "s":{"0":1,"1":1,"2":0},
              "statementMap":{"0":{},"1":{},"2":{}},
              "f":{"0":2},"fnMap":{"0":{}},
              "b":{"0":[3,0]},"branchMap":{"0":{"locations":[{},{}]}}
            }}"#;
        let report = parse_istanbul_coverage(final_json).unwrap();
        assert_eq!((report.statements.total, report.statements.covered), (3, 2));
        assert_eq!((report.branches.total, report.branches.covered), (2, 1));
        assert_eq!((report.functions.total, report.functions.covered), (1, 1));
        assert_eq!(report.files.len(), 1);

        // coverage-summary.json (reporters=json-summary) — "root" is not a file
        let summary_json = r#"{"total":{"lines":{"total":100,"covered":90,"pct":90},
              "statements":{"total":110,"covered":99,"pct":90},"branches":{"total":20,"covered":10,"pct":50},
              "functions":{"total":10,"covered":5,"pct":50}},
            "root":"/abs/project",
            "/abs/src/a.ts":{"lines":{"total":60,"covered":58,"pct":96.66},
              "statements":{"total":60,"covered":58,"pct":96.66},
              "branches":{"total":12,"covered":6,"pct":50},
              "functions":{"total":6,"covered":3,"pct":50}},
            "/abs/src/b.ts":{"lines":{"total":40,"covered":32,"pct":80},
              "statements":{"total":50,"covered":41,"pct":82},
              "branches":{"total":8,"covered":4,"pct":50},
              "functions":{"total":4,"covered":2,"pct":50}}}"#;
        let report = parse_istanbul_summary_coverage(summary_json).unwrap();
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.lines.total, 100);
        assert!((report.lines.percentage - 90.0).abs() < 0.01);
        assert!((report.branches.percentage - 50.0).abs() < 0.01);
        assert_eq!(report.files[0].path, "/abs/src/a.ts");

        // pytest-cov --cov-report=json
        let pytest_json = r#"{"files":{
            "src/a.py":{"summary":{"num_statements":50,"covered_lines":40}},
            "src/b.py":{"summary":{"num_statements":50,"covered_lines":10}}}}"#;
        let report = parse_pytest_coverage(pytest_json).unwrap();
        assert_eq!((report.lines.total, report.lines.covered), (100, 50));
        assert_eq!(report.files.len(), 2);

        // cargo llvm-cov --json
        let llvm_json = r#"{"data":[{"files":[
            {"filename":"src/main.rs","summary":{"lines":{"count":20,"covered":15}}},
            {"filename":"src/lib.rs","summary":{"lines":{"count":30,"covered":30}}}]}]}"#;
        let report = parse_cargo_coverage(llvm_json).unwrap();
        assert_eq!((report.lines.total, report.lines.covered), (50, 45));
        assert_eq!(report.files.len(), 2);
    }

    const VITEST_REPORT: &str = r#"{"numTotalTests":4,"numPassedTests":2,"numFailedTests":1,
      "testResults":[
        {"name":"D:/proj/src/a.test.ts","assertionResults":[
          {"fullName":"math adds","status":"passed","duration":12.4,"failureMessages":[]},
          {"fullName":"math divides","status":"failed","duration":3.6,
           "failureMessages":["AssertionError: expected 1 to be 2\n    at Object.<anonymous> (src/a.test.ts:9:1)\n    at runMicrotasks"]},
          {"fullName":"math skips","status":"skipped","duration":0,"failureMessages":[]}
        ]},
        {"name":"D:/proj/src/b.test.ts","assertionResults":[
          {"ancestorTitles":["async"],"title":"awaits","status":"passed","duration":1005,"failureMessages":[]}
        ]}
      ]}"#;

    #[test]
    fn jest_style_report_produces_case_detail() {
        let suites = parse_jest_style_results(VITEST_REPORT).unwrap();
        assert_eq!(suites.len(), 2);
        assert_eq!(suites[0].name, "a.test.ts");
        assert_eq!(suites[0].status, "failed");
        assert_eq!(suites[1].status, "passed");

        let cases: Vec<&TestCase> = suites.iter().flat_map(|s| s.tests.iter()).collect();
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[0].name, "math adds");
        assert_eq!(cases[0].status, "passed");
        assert_eq!(cases[0].duration, 12);
        // ancestorTitles + title are used when fullName is missing.
        assert_eq!(cases[3].name, "async > awaits");
        assert_eq!(cases[3].duration, 1005);

        let err = cases[1].error.as_ref().expect("a failing case carries an error");
        assert_eq!(err.message, "AssertionError: expected 1 to be 2");
        assert!(err.stack.contains("src/a.test.ts:9:1"), "stack: {}", err.stack);
        assert_eq!(suite_counts(&suites), (4, 2, 1, 1));
        assert_eq!(suites[0].duration, 12 + 4 + 0);
    }

    #[test]
    fn a_report_file_wins_over_stdout_scraping() {
        if !node_available() {
            return;
        }
        let dir = scratch("report-file");
        std::fs::write(dir.join("vitest-results.json"), VITEST_REPORT).unwrap();
        // The scraped summary would say 9 passed; the report says otherwise.
        write_script(&dir, "t.js", "console.log('Tests  9 passed (9)');");
        let conn = db();

        let result = run(&conn, &dir, "p-report", None, Duration::from_secs(60)).unwrap();

        assert_eq!((result.total_tests, result.passed, result.failed, result.skipped), (4, 2, 1, 1));
        assert_eq!(result.status, "failed");
        assert_eq!(result.suites.len(), 2);
        assert_eq!(result.suites[0].tests.len(), 3);
        // The parsed suites are persisted too, so the history row can show them again.
        let stored: String = conn
            .lock()
            .unwrap()
            .query_row("SELECT suites FROM test_runs", [], |r| r.get(0))
            .unwrap();
        assert!(stored.contains("math divides"), "stored: {}", stored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_json_stdout_is_recognised() {
        if !node_available() {
            return;
        }
        let dir = scratch("report-stdout");
        write_script(
            &dir,
            "t.js",
            &format!("console.log({});", serde_json::to_string(VITEST_REPORT).unwrap()),
        );
        let conn = db();

        let result = run(&conn, &dir, "p-stdout", None, Duration::from_secs(60)).unwrap();

        assert_eq!(result.total_tests, 4);
        assert_eq!(result.suites.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_without_a_report_keeps_scraped_counts_and_empty_suites() {
        if !node_available() {
            return;
        }
        let dir = scratch("no-report");
        write_script(&dir, "t.js", "console.log('Tests  3 passed (3)');");
        let conn = db();

        let result = run(&conn, &dir, "p-plain", None, Duration::from_secs(60)).unwrap();

        assert_eq!(result.total_tests, 3);
        assert!(result.suites.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_history_row_reads_back_its_stored_run() {
        if !node_available() {
            return;
        }
        let dir = scratch("readback");
        std::fs::write(dir.join("vitest-results.json"), VITEST_REPORT).unwrap();
        write_script(&dir, "t.js", "console.log('done');");
        let conn = db();

        let result = run(&conn, &dir, "p-readback", None, Duration::from_secs(60)).unwrap();
        let stored = run_by_id_sync(&conn.lock().unwrap(), &result.id).unwrap();

        assert_eq!(stored.id, result.id);
        assert_eq!(stored.status, result.status);
        assert_eq!((stored.passed, stored.failed, stored.skipped), (2, 1, 1));
        assert_eq!(stored.suites.len(), 2);
        assert_eq!(stored.output, result.output);
        assert!(run_by_id_sync(&conn.lock().unwrap(), "nope").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_keeps_only_the_newest_runs() {
        let conn = db();
        add_project_sync(
            &conn.lock().unwrap(),
            project(Path::new("."), "p-keep", "noop", "custom"),
        )
        .unwrap();
        for i in 0..5 {
            record_run(
                &conn,
                &format!("run-{}", i),
                "p-keep",
                "2026-09-20T00:00:00Z",
                &format!("2026-09-20T00:00:0{}Z", i),
                i as u64 * 10,
                "success",
                1,
                1,
                0,
                0,
                "out",
                "[]",
            )
            .unwrap();
        }
        // std::sync::Mutex is not reentrant: take the lock per query instead of
        // holding a guard across calls that lock it themselves.
        let select_ids = |sql: &str| -> Vec<String> {
            let guard = conn.lock().unwrap();
            let mut stmt = guard.prepare(sql).unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            let ids: Vec<String> = rows.filter_map(Result::ok).collect();
            ids
        };
        let count = |sql: &str| -> i64 {
            let guard = conn.lock().unwrap();
            let n: i64 = guard.query_row(sql, [], |r| r.get(0)).unwrap();
            n
        };

        // record_run's own retention is far higher than 5, so nothing is dropped yet.
        assert_eq!(
            select_ids("SELECT id FROM test_runs WHERE project_id = 'p-keep'").len(),
            5
        );

        {
            let guard = conn.lock().unwrap();
            prune_run_history(&guard, "p-keep", 2).unwrap();
            prune_history(&guard, "p-keep", 2).unwrap();
        }
        assert_eq!(
            select_ids("SELECT id FROM test_runs WHERE project_id = 'p-keep' ORDER BY id"),
            vec!["run-3".to_string(), "run-4".to_string()]
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM test_history WHERE project_id = 'p-keep'"),
            2
        );

        // Trimming one project must leave another project's rows alone.
        add_project_sync(
            &conn.lock().unwrap(),
            project(Path::new("."), "p-other", "noop", "custom"),
        )
        .unwrap();
        record_run(&conn, "run-x", "p-other", "t", "t", 1, "success", 1, 1, 0, 0, "o", "[]").unwrap();
        {
            let guard = conn.lock().unwrap();
            prune_run_history(&guard, "p-keep", 1).unwrap();
        }
        assert_eq!(
            count("SELECT COUNT(*) FROM test_runs WHERE project_id = 'p-other'"),
            1
        );
    }
}
