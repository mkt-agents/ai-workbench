// CLI: cargo run --bin cursor_switch_test
// Tests profile-based account switching (will quit and relaunch Cursor).

fn main() {
    let personal = std::env::var("WT_PERSONAL_ID").unwrap_or_else(|_| "1788857782889".to_string());
    let company = std::env::var("WT_COMPANY_ID").unwrap_or_else(|_| "1788858681604".to_string());

    eprintln!(
        "=== Cursor 切换自测 ===\n个人: {}\n公司: {}\n（将关闭并重启 Cursor）\n",
        personal, company
    );

    match ai_workbench::cursor::run_cursor_switch_self_test(&personal, &company) {
        Ok(summary) => {
            println!("{}", summary);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("FAILED: {}", e);
            std::process::exit(1);
        }
    }
}
