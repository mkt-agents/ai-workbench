// CLI: cargo run --bin dump_state -- [default|shared|active|profile:<id>] [key]
//  - no args           → list all keys in default Cursor state.vscdb
//  - one arg (target)  → list all keys in that target's state.vscdb
//  - two args          → dump value of <key> in <target>'s state.vscdb
//  - first arg "tables" → list all tables in default state.vscdb

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = args.first().cloned().or(Some("default".to_string()));
    let target_ref = target.as_deref();

    // Special mode: list tables
    if target_ref == Some("tables") {
        let path = std::env::args().nth(2).unwrap_or_else(|| "default".to_string());
        return run_list_tables(&path);
    }

    match ai_workbench::cursor::dump_state_keys(target_ref.map(|s| s.to_string())) {
        Ok(keys) => {
            if let Some(key) = args.get(1) {
                // dump single value
                match ai_workbench::cursor::dump_state_value(
                    target_ref.map(|s| s.to_string()),
                    key.clone(),
                ) {
                    Ok(val) => {
                        println!("=== value of {key} (target={target_ref:?}) ===");
                        // Truncate very long values for terminal readability.
                        if val.len() > 4000 {
                            println!("{}\n...[truncated, total {} bytes]", &val[..4000], val.len());
                        } else {
                            println!("{val}");
                        }
                    }
                    Err(e) => {
                        eprintln!("读取 value 失败: {e}");
                        std::process::exit(2);
                    }
                }
                return;
            }
            // list keys
            println!("=== keys in target={target_ref:?} (total: {}) ===", keys.len());
            for k in keys {
                println!("{k}");
            }
        }
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn run_list_tables(target: &str) {
    use rusqlite::OpenFlags;
    use rusqlite::Connection;
    let path: PathBuf = match target {
        "default" => dirs_default_state_vscdb(),
        "shared" => dirs_shared_state_vscdb(),
        _ => {
            eprintln!("未知 target: {target}");
            std::process::exit(1);
        }
    };
    if !path.exists() {
        eprintln!("数据库不存在: {}", path.display());
        std::process::exit(1);
    }
    let uri = format!("file:///{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let conn = Connection::open_with_flags(&uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
        .unwrap_or_else(|e| { eprintln!("打开数据库失败: {e}"); std::process::exit(2) });
    let mut stmt = conn.prepare("SELECT name, type FROM sqlite_master WHERE type IN ('table','view') ORDER BY name")
        .unwrap_or_else(|e| { eprintln!("准备查询失败: {e}"); std::process::exit(3) });
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).unwrap_or_else(|e| { eprintln!("查询失败: {e}"); std::process::exit(4) });
    println!("=== tables in {target} ({}) ===", path.display());
    for row in rows {
        if let Ok((name, typ)) = row {
            println!("{typ:8} {name}");
        }
    }

    // Dump row counts and a sample for each table
    for table_name in ["ItemTable", "composerHeaders", "cursorDiskKV"] {
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {table_name}"),
            [],
            |r| r.get(0),
        ).unwrap_or(0);
        println!("\n--- {table_name} (rows: {count}) ---");
        // Show schema
        if let Ok(mut cols) = conn.prepare(&format!("PRAGMA table_info({table_name})")) {
            if let Ok(rows) = cols.query_map([], |r| {
                Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            }) {
                print!("columns: ");
                for cr in rows.flatten() {
                    print!("{}({}) ", cr.0, cr.1);
                }
                println!();
            }
        }
        // For cursorDiskKV: list distinct key prefixes (before first ':' or '-' or end)
        if table_name == "cursorDiskKV" {
            if let Ok(mut s) = conn.prepare(&format!("SELECT key FROM {table_name}")) {
                if let Ok(rows) = s.query_map([], |r| r.get::<_, String>(0)) {
                    let mut prefixes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                    for r in rows.flatten() {
                        let prefix = r.split([':', '-']).next().unwrap_or(&r).to_string();
                        *prefixes.entry(prefix).or_insert(0) += 1;
                    }
                    let mut sorted: Vec<_> = prefixes.into_iter().collect();
                    sorted.sort_by(|a, b| b.1.cmp(&a.1));
                    println!("  key prefixes (top 30):");
                    for (p, c) in sorted.into_iter().take(30) {
                        println!("    {p}: {c}");
                    }
                }
            }
            continue;
        }
        // For composerHeaders: list distinct workspaceId counts
        if table_name == "composerHeaders" {
            if let Ok(mut s) = conn.prepare(&format!("SELECT workspaceId, COUNT(*) FROM {table_name} GROUP BY workspaceId ORDER BY COUNT(*) DESC LIMIT 10")) {
                if let Ok(rows) = s.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                }) {
                    println!("  workspaceId counts:");
                    for r in rows.flatten() {
                        println!("    {}: {}", r.0, r.1);
                    }
                }
            }
            continue;
        }
        // For ItemTable: show first 3 rows
        if let Ok(mut s) = conn.prepare(&format!("SELECT * FROM {table_name} LIMIT 3")) {
            if let Ok(rows) = s.query_map([], |row| {
                let mut vals = Vec::new();
                for i in 0..row.as_ref().column_count() {
                    let v: String = row.get::<_, String>(i).unwrap_or_else(|_| "<bin>".to_string());
                    let v = if v.len() > 80 { format!("{}...", &v[..80]) } else { v };
                    vals.push(v);
                }
                println!("  row: {}", vals.join(" | "));
                Ok(())
            }) {
                for _ in rows.flatten() {}
            }
        }
    }
}

fn dirs_default_state_vscdb() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    std::path::Path::new(&appdata).join("Cursor").join("User").join("globalStorage").join("state.vscdb")
}

fn dirs_shared_state_vscdb() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    std::path::Path::new(&appdata)
        .join("com.ai-workbench.app")
        .join("cursor-shared")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}
