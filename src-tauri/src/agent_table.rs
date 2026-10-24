//! 表格查询：把 CSV / XLSX 读进内存 SQLite，只允许单条 `SELECT`。
//!
//! 为什么不引第三方向量引擎：`rusqlite` 已经在依赖里，聚合、分组、窗口函数都够用；
//! 只读校验与行数上限也比引一个新引擎更容易收口。

use std::path::Path;

/// 单次查询返回的最大行数。
pub(crate) const ROW_LIMIT: usize = 500;

/// 打开表 → 建内存库 → 执行一条只读查询 → 返回 TSV。
pub(crate) fn query(path: &Path, sql: &str, limit: usize) -> Result<String, String> {
    check_select(sql)?;
    let (header, rows) = crate::agent_docs::read_rows(path, crate::agent_docs::MAX_TABLE_ROWS)?;
    let conn = rusqlite::Connection::open_in_memory().map_err(|e| format!("建临时库失败：{e}"))?;
    let columns = header
        .iter()
        .map(|name| format!("\"{}\" TEXT", name.replace('"', "")))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(&format!("CREATE TABLE t ({columns})"), [])
        .map_err(|e| format!("建表失败：{e}"))?;
    let placeholders = vec!["?"; header.len()].join(", ");
    {
        let mut insert = conn
            .prepare(&format!("INSERT INTO t VALUES ({placeholders})"))
            .map_err(|e| format!("准备插入失败：{e}"))?;
        for row in &rows {
            // 行的列数可能和表头不一致（CSV 少写一列很常见）：补齐或截断，
            // 否则 SQLite 会报 "wrong number of parameters" 直接整条查询失败。
            let mut cells: Vec<&str> = row.iter().take(header.len()).map(String::as_str).collect();
            while cells.len() < header.len() {
                cells.push("");
            }
            insert
                .execute(rusqlite::params_from_iter(cells.iter()))
                .map_err(|e| format!("插入数据失败：{e}"))?;
        }
    }

    let mut statement = conn.prepare(sql).map_err(|e| format!("SQL 有语法错误：{e}"))?;
    let column_count = statement.column_count();
    let names: Vec<String> = (0..column_count)
        .map(|i| statement.column_name(i).unwrap_or("?").to_string())
        .collect();
    let limit = limit.clamp(1, ROW_LIMIT);
    let mut result = vec![names.join("\t")];
    let mut total = 0usize;
    let mut query_rows = statement.query([]).map_err(|e| format!("查询失败：{e}"))?;
    while let Some(row) = query_rows.next().map_err(|e| format!("读结果失败：{e}"))? {
        total += 1;
        if total > limit {
            continue;
        }
        let mut cells = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let value = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => String::new(),
                Ok(rusqlite::types::ValueRef::Integer(n)) => n.to_string(),
                Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                Ok(rusqlite::types::ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
                Ok(rusqlite::types::ValueRef::Blob(_)) => "<blob>".into(),
                Err(_) => String::new(),
            };
            cells.push(value.replace(['\t', '\n'], " "));
        }
        result.push(cells.join("\t"));
    }
    if total > limit {
        result.push(format!("…（共 {total} 行，只返回前 {limit} 行，用 LIMIT / 聚合收窄）"));
    }
    Ok(result.join("\n"))
}

/// 只读校验：单条 SELECT / WITH，禁止一切写操作与多语句。
fn check_select(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err("SQL 为空".into());
    }
    if trimmed.contains(';') {
        return Err("一次只能执行一条查询".into());
    }
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("select") && !lower.starts_with("with") {
        return Err("只允许 SELECT 查询（表已经在 `t` 里，不用再查文件名）".into());
    }
    // 按标识符切词再比关键字：用 `contains` 会把 `updated_at` 这种列名当成 UPDATE 拒掉。
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .collect();
    for banned in [
        "attach", "detach", "pragma", "insert", "update", "delete", "drop", "create", "alter", "vacuum", "replace",
        "into",
    ] {
        if tokens.contains(&banned) {
            return Err(format!("查询里不能出现 `{banned}`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_csv(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-table-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn aggregates_over_the_imported_table() {
        let path = temp_csv("agg", "name,amount\n甲,10\n乙,20\n丙,30\n");
        let out = query(&path, "SELECT count(*) AS n, sum(amount) AS total FROM t", 100).unwrap();
        assert!(out.starts_with("n\ttotal"));
        assert!(out.contains("3\t60"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn short_rows_are_padded_instead_of_failing() {
        // 表头 2 列、第二行只有 1 列：补齐成空串，查询仍然要能跑
        let path = temp_csv("ragged", "name,amount\n甲\n乙,20\n");
        let out = query(&path, "SELECT count(*) AS n FROM t", 10).unwrap();
        assert!(out.contains("2"), "{out}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn writes_are_refused() {
        for sql in [
            "DELETE FROM t",
            "SELECT * FROM t; DROP TABLE t",
            "SELECT * FROM t INTO OUTFILE 'x'",
            "PRAGMA table_info(t)",
            "",
        ] {
            assert!(check_select(sql).is_err(), "{sql} 不该通过");
        }
        assert!(check_select("SELECT * FROM t").is_ok());
        // 列名里带关键字不算写操作（`updated_at` 不是 UPDATE）
        assert!(check_select("SELECT updated_at, created_at FROM t").is_ok());
        assert!(check_select("with x as (select 1) select * from x").is_ok());
    }

    #[test]
    fn row_limit_is_announced() {
        let path = temp_csv("limit", "n\n1\n2\n3\n4\n5\n");
        let out = query(&path, "SELECT n FROM t", 2).unwrap();
        assert!(out.contains("只返回前 2 行"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
