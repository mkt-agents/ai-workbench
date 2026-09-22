//! Schema migration for the test-assistant tables.
//!
//! rusqlite only, so it can be compiled and *run* from a scratch crate — which matters
//! here more than anywhere: `CREATE TABLE IF NOT EXISTS` never adds a column to an
//! existing database, so without this an upgraded app would query columns that were
//! never created, and the failure would surface as a confusing runtime error for
//! whoever already had projects registered.

use rusqlite::Connection;

/// Add the columns introduced after the first release. Idempotent: run on every start.
pub fn migrate(conn: &Connection) -> Result<(), String> {
    ensure_column(conn, "test_projects", "last_error_kind", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "test_runs", "error_kind", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "change_reports", "delta_coverage", "TEXT")?;
    ensure_column(conn, "change_reports", "accepted_at", "TEXT")?;
    ensure_column(conn, "change_reports", "ai_warnings", "TEXT NOT NULL DEFAULT '[]'")?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<bool, String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .map_err(|e| format!("读取 {} 表结构失败: {}", table, e))?;
    if exists {
        return Ok(false);
    }
    conn.execute(
        &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, declaration),
        [],
    )
    .map_err(|e| format!("为 {}.{} 加列失败: {}", table, column, e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_SCHEMA: &str = r#"
CREATE TABLE test_projects (
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
CREATE TABLE test_runs (
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
    suites TEXT NOT NULL
);
CREATE TABLE change_reports (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    base TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    branch TEXT NOT NULL DEFAULT '',
    head TEXT NOT NULL DEFAULT '',
    files INTEGER NOT NULL DEFAULT 0,
    adds INTEGER NOT NULL DEFAULT 0,
    dels INTEGER NOT NULL DEFAULT 0,
    untested INTEGER NOT NULL DEFAULT 0,
    data TEXT NOT NULL,
    patch TEXT NOT NULL DEFAULT '',
    ai TEXT
);
"#;

    fn pre_migration_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(OLD_SCHEMA).unwrap();
        db.execute(
            "INSERT INTO test_projects (id, name, path, type, framework, test_command, created_at, updated_at)
             VALUES ('p1','aio.ui','D:/repo/aio.ui','backend','maven','mvn -B test','t','t')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO test_runs (id, project_id, started_at, completed_at, duration_ms, status, total_tests, passed, failed, skipped, output, suites)
             VALUES ('r1','p1','a','b',1,'error',0,0,0,0,'log','[]')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO change_reports (id, project_id, base, source, created_at, data)
             VALUES ('cr-1','p1','','uncommitted','t','{}')",
            [],
        )
        .unwrap();
        db
    }

    #[test]
    fn an_existing_database_gains_the_new_columns_without_losing_rows() {
        let db = pre_migration_db();
        migrate(&db).unwrap();

        let columns: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('test_projects')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(columns.contains(&"last_error_kind".to_string()), "{:?}", columns);
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT COUNT(*) FROM test_projects", [], |r| r.get(0))
                .unwrap(),
            1,
            "migration must not touch data"
        );
        assert_eq!(
            db.query_row::<String, _, _>("SELECT last_error_kind FROM test_projects WHERE id = 'p1'", [], |r| r.get(0))
                .unwrap(),
            "",
            "existing rows get the default, not NULL"
        );
        assert_eq!(
            db.query_row::<String, _, _>("SELECT error_kind FROM test_runs WHERE id = 'r1'", [], |r| r.get(0))
                .unwrap(),
            ""
        );
        let (delta, accepted, warnings): (Option<String>, Option<String>, String) = db
            .query_row(
                "SELECT delta_coverage, accepted_at, ai_warnings FROM change_reports WHERE id = 'cr-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(delta.is_none() && accepted.is_none(), "nullable columns start empty");
        assert_eq!(warnings, "[]", "the JSON list column gets its default, not NULL");
    }

    #[test]
    fn running_it_twice_is_a_no_op() {
        let db = pre_migration_db();
        migrate(&db).unwrap();
        let again = migrate(&db);
        assert!(again.is_ok(), "second run must not fail on duplicate columns: {:?}", again);
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT COUNT(*) FROM test_runs", [], |r| r.get(0)).unwrap(),
            1
        );
    }

    #[test]
    fn a_table_that_does_not_exist_reports_instead_of_panicking() {
        let db = Connection::open_in_memory().unwrap();
        let err = migrate(&db).unwrap_err();
        assert!(err.contains("test_projects"), "got: {}", err);
    }
}
