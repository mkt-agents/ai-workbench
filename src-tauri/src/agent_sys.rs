//! AI hub agent system tools: open files / URLs, clipboard, processes and ports,
//! plus the workbench's own abilities (hosts, Cloudflare tunnels, runtime versions).

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::agent_fs::{arg_str, Scope};

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("序列化失败: {e}"))
}

/// URL schemes `open_target` hands to the shell; anything else (custom protocol handlers) is refused.
pub(crate) fn is_web_url(target: &str) -> bool {
    let lower = target.trim().to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

pub(crate) fn open_target(scope: &Scope, args: &Value) -> Result<String, String> {
    let target = arg_str(args, "target");
    if target.is_empty() {
        return Err("缺少要打开的文件、文件夹或网址".into());
    }
    let resolved = if is_web_url(&target) {
        target.clone()
    } else if target.contains("://") {
        return Err("只支持 http / https / mailto 网址和本机文件路径".into());
    } else {
        let path = scope.resolve(&target)?;
        if !path.exists() {
            return Err(format!("路径不存在：{}", path.display()));
        }
        path.display().to_string()
    };
    shell_open(&resolved)?;
    Ok(format!("已打开 {resolved}"))
}

#[cfg(windows)]
fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let verb = wide("open");
    let file = wide(target);
    let code = unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), None, None, SW_SHOWNORMAL) };
    // ShellExecute reports success with a value greater than 32.
    if code.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("打开失败（代码 {}）", code.0 as isize))
    }
}

#[cfg(not(windows))]
fn shell_open(target: &str) -> Result<(), String> {
    let _ = target;
    Err("当前系统不支持".into())
}

pub(crate) fn clipboard_read() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let text = cb.get_text().map_err(|_| "剪贴板里没有文本".to_string())?;
    Ok(crate::agent_proc::clip_head_tail(&text, 8_000))
}

pub(crate) fn clipboard_write(args: &Value) -> Result<String, String> {
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())?;
    Ok(format!("已写入剪贴板（{} 字）", text.chars().count()))
}

#[derive(Debug, PartialEq)]
pub(crate) struct ProcRow {
    pub name: String,
    pub pid: u32,
    pub mem_kb: u64,
}

/// `tasklist /FO CSV /NH` rows: `"name","pid","session","#","12,345 K"`.
pub(crate) fn parse_tasklist(text: &str) -> Vec<ProcRow> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let inner = line.strip_prefix('"')?.strip_suffix('"')?;
            let cols: Vec<&str> = inner.split("\",\"").collect();
            if cols.len() < 5 {
                return None;
            }
            let pid = cols[1].trim().parse().ok()?;
            let mem_kb = cols[4].chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
            Some(ProcRow { name: cols[0].to_string(), pid, mem_kb })
        })
        .collect()
}

fn tasklist_rows() -> Result<Vec<ProcRow>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| e.to_string())?;
        Ok(parse_tasklist(&crate::agent_proc::decode_output(&out.stdout)))
    }
    #[cfg(not(windows))]
    {
        Err("当前系统不支持".into())
    }
}

pub(crate) fn list_processes(args: &Value) -> Result<String, String> {
    let filter = arg_str(args, "name").to_lowercase();
    let mut rows: Vec<ProcRow> = tasklist_rows()?
        .into_iter()
        .filter(|r| filter.is_empty() || r.name.to_lowercase().contains(&filter))
        .collect();
    rows.sort_by(|a, b| b.mem_kb.cmp(&a.mem_kb));
    let total = rows.len();
    let lines: Vec<String> = rows
        .iter()
        .take(60)
        .map(|r| format!("{:>7}  {:>8} MB  {}", r.pid, r.mem_kb / 1024, r.name))
        .collect();
    Ok(format!("共 {total} 个进程（按内存排序，最多列 60 个）\n    PID       内存  名称\n{}", lines.join("\n")))
}

pub(crate) fn kill_process(args: &Value) -> Result<String, String> {
    let rows = tasklist_rows()?;
    let pid_arg = args.get("pid").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    let name = arg_str(args, "name").to_lowercase();
    let targets: Vec<&ProcRow> = match pid_arg {
        Some(pid) => rows.iter().filter(|r| r.pid as u64 == pid).collect(),
        None if !name.is_empty() => rows
            .iter()
            .filter(|r| {
                let n = r.name.to_lowercase();
                n == name || n == format!("{name}.exe")
            })
            .collect(),
        None => return Err("要指定 pid 或进程名 name".into()),
    };
    if targets.is_empty() {
        return Err("没有找到这个进程".into());
    }
    let me = std::process::id();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for t in targets {
        if t.pid == me {
            failed.push(format!("{} 是天玑自己，不能结束", t.pid));
            continue;
        }
        if !crate::port_commands::protection_for(t.pid, &t.name).is_empty() {
            failed.push(format!("{}（{}）是系统关键进程，已拒绝", t.pid, t.name));
            continue;
        }
        match crate::port_commands::kill_pid(t.pid) {
            Ok(()) => done.push(format!("{}（{}）", t.pid, t.name)),
            Err(e) => failed.push(e),
        }
    }
    Ok(format!(
        "已结束：{}{}",
        if done.is_empty() { "无".into() } else { done.join("、") },
        if failed.is_empty() { String::new() } else { format!("\n未结束：{}", failed.join("；")) }
    ))
}

fn ports_arg(args: &Value) -> Vec<u16> {
    match args.get("port") {
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_u64()).map(|p| p as u16).collect(),
        Some(v) => v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).map(|p| vec![p as u16]).unwrap_or_default(),
        None => Vec::new(),
    }
}

pub(crate) async fn find_port(args: &Value) -> Result<String, String> {
    let ports = ports_arg(args);
    if ports.is_empty() {
        return Err("缺少端口号 port".into());
    }
    let listeners = crate::port_commands::find_port_listeners(ports).await?;
    if listeners.is_empty() {
        return Ok("没有进程在监听这些端口".into());
    }
    Ok(to_json(&listeners))
}

pub(crate) async fn kill_port(args: &Value) -> Result<String, String> {
    let ports = ports_arg(args);
    let Some(&port) = ports.first() else {
        return Err("缺少端口号 port".into());
    };
    let listeners = crate::port_commands::find_port_listeners(vec![port]).await?;
    let pids: Vec<u32> = listeners.iter().map(|l| l.pid).collect();
    if pids.is_empty() {
        return Ok(format!("端口 {port} 没有被占用"));
    }
    let result = crate::port_commands::kill_port_listeners(port, pids).await?;
    Ok(to_json(&result))
}

/// Proposed hosts content vs the current file, for the approval card.
pub(crate) fn hosts_preview(args: &Value) -> Option<String> {
    if arg_str(args, "action") != "write" {
        return None;
    }
    let next = args.get("content").and_then(|v| v.as_str())?;
    let current = crate::hosts_commands::read_system_hosts().unwrap_or_default();
    Some(crate::agent_fs::unified_diff("hosts", &current, next))
}

pub(crate) async fn hosts(args: &Value) -> Result<String, String> {
    match arg_str(args, "action").as_str() {
        "read" => crate::hosts_commands::read_system_hosts(),
        "write" => {
            let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
                return Err("write 需要完整的新 hosts 内容 content（先 read 再整体改写）".into());
            };
            if !crate::hosts_commands::is_admin() {
                return Err("写 hosts 需要管理员权限：请用户在 Hosts 页点「以管理员身份重启」后再试。".into());
            }
            crate::hosts_commands::write_system_hosts(content.to_string()).await
        }
        other => Err(format!("hosts 不支持 action={other}，可用 read / write")),
    }
}

pub(crate) async fn tunnel(app: &AppHandle, args: &Value) -> Result<String, String> {
    use crate::cloudflared_commands as cf;
    let action = arg_str(args, "action");
    let status = cf::cloudflared_status().await?;
    if action != "status" && !status.installed {
        return Err(format!("本机没有可用的 cloudflared：{}。请用户在内网穿透页安装。", status.message));
    }
    match action.as_str() {
        "status" => {
            let tunnels = cf::cloudflared_tunnel_status(app.state::<cf::CloudflaredState>());
            Ok(to_json(&json!({ "cloudflared": status, "tunnels": tunnels })))
        }
        "start_quick" => {
            let local_url = arg_str(args, "local_url");
            if local_url.is_empty() {
                return Err("start_quick 需要本地地址 local_url，比如 http://localhost:3000".into());
            }
            let started = cf::cloudflared_start_quick_tunnel(app.clone(), local_url, None).await?;
            Ok(to_json(&started))
        }
        "stop" => {
            let id = arg_str(args, "id");
            if id.is_empty() {
                return Err("stop 需要隧道 id（先用 status 查看）".into());
            }
            cf::cloudflared_stop_tunnel(app.state::<cf::CloudflaredState>(), id).await
        }
        other => Err(format!("tunnel 不支持 action={other}，可用 status / start_quick / stop")),
    }
}

pub(crate) async fn runtime(args: &Value) -> Result<String, String> {
    use crate::runtime_commands as rt;
    let kind = arg_str(args, "kind");
    if kind.is_empty() {
        return Err("缺少 kind：node / jdk / maven / python".into());
    }
    match arg_str(args, "action").as_str() {
        "list" => Ok(to_json(&rt::list_runtime_versions(kind).await?)),
        "active" => Ok(to_json(&rt::get_active_runtime(kind).await?)),
        "switch" => {
            let mut path = arg_str(args, "path");
            let version = arg_str(args, "version");
            if path.is_empty() && !version.is_empty() {
                let list = rt::list_runtime_versions(kind.clone()).await?;
                let hit = list
                    .iter()
                    .find(|v| v.version == version)
                    .or_else(|| list.iter().find(|v| v.version.starts_with(&version)));
                path = hit.map(|v| v.path.clone()).ok_or_else(|| format!("没有找到 {kind} {version}，先用 list 看可选版本"))?;
            }
            if path.is_empty() {
                return Err("switch 需要 version 或 path".into());
            }
            let result = rt::switch_runtime(kind, path).await?;
            Ok(format!("{}（版本 {}{}）", result.message, result.version, if result.verified { "，已验证" } else { "" }))
        }
        other => Err(format!("runtime 不支持 action={other}，可用 list / active / switch")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasklist_csv_is_parsed() {
        let text = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\"chrome.exe\",\"1234\",\"Console\",\"1\",\"123,456 K\"\r\nbad line\r\n";
        let rows = parse_tasklist(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], ProcRow { name: "chrome.exe".into(), pid: 1234, mem_kb: 123_456 });
    }

    #[test]
    fn only_web_urls_go_to_the_shell() {
        assert!(is_web_url("https://example.com"));
        assert!(is_web_url("MAILTO:a@b.c"));
        assert!(!is_web_url("ms-settings:display"));
        assert!(!is_web_url("D:\\a.txt"));
    }

    #[test]
    fn port_argument_accepts_number_string_or_list() {
        assert_eq!(ports_arg(&json!({ "port": 3000 })), vec![3000]);
        assert_eq!(ports_arg(&json!({ "port": "8080" })), vec![8080]);
        assert_eq!(ports_arg(&json!({ "port": [1, 2] })), vec![1, 2]);
        assert!(ports_arg(&json!({})).is_empty());
    }
}
