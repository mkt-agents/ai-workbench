//! Shared port / process primitives: `netstat` parsing, port→PID lookup, and kill.
//!
//! Two consumers depend on this module: `dsh_commands.rs` (stop the DeepSeek service and
//! release its port) and the 快捷应用 port panel. Deleting it breaks both.
//!
//! Parsing is kept pure (`parse_netstat_line` / `matching_listeners`) so the exact-port,
//! IPv6 and PID-0 rules are unit-testable without spawning a process.

use std::net::TcpListener;
use std::thread;
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
use std::process::Command;

#[cfg(target_os = "windows")]
use crate::app_launcher_commands::{decode_gbk, hidden_command, invalidate_tasklist_cache};

/// One data row of `netstat -ano`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetRow {
    /// `"TCP"` | `"UDP"`.
    pub proto: String,
    /// Verbatim local address, e.g. `0.0.0.0`, `[::]`, `[::1]`. A `[` prefix means IPv6.
    pub local_addr: String,
    pub local_port: u16,
    /// `LISTENING` / `ESTABLISHED` / … — empty for UDP, which has no state column.
    pub state: String,
    pub pid: u32,
}

impl NetRow {
    /// Does this row describe a socket that *holds* the port (as opposed to a client
    /// connection or a lingering TIME_WAIT)? PID 0 is the System Idle process: it owns
    /// TIME_WAIT rows and the SMB backlog, and is never a kill target.
    pub(crate) fn is_listener(&self) -> bool {
        if self.pid == 0 {
            return false;
        }
        match self.proto.as_str() {
            "TCP" => self.state == "LISTENING",
            "UDP" => true,
            _ => false,
        }
    }
}

/// Parse one `netstat -ano` line.
///
/// TCP is `Proto Local Foreign State PID`; UDP omits the state column, so a positional
/// parse has to branch on the protocol rather than assume five fields.
pub(crate) fn parse_netstat_line(line: &str) -> Option<NetRow> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let (proto, local, state, pid_str) = match parts.as_slice() {
        [proto, local, _, state, pid] if *proto == "TCP" => (*proto, *local, *state, *pid),
        [proto, local, _, pid] if *proto == "UDP" => (*proto, *local, "", *pid),
        _ => return None,
    };
    // `rsplit_once` takes the *last* colon, so the bracketed IPv6 forms (`[::]:135`)
    // never yield a bogus port.
    let (local_addr, port_str) = local.rsplit_once(':')?;
    Some(NetRow {
        proto: proto.to_string(),
        local_addr: local_addr.to_string(),
        local_port: port_str.parse().ok()?,
        state: state.to_string(),
        pid: pid_str.parse().ok()?,
    })
}

/// Rows whose *local* port is one of `ports` and which actually hold it.
///
/// Matching the local port only — never a substring of the line — is what keeps a query
/// for 3080 from hitting `0.0.0.0:30800`, and an ESTABLISHED row whose *remote* end is
/// on 8080 from being killed as if it were the server.
pub(crate) fn matching_listeners(text: &str, ports: &[u16], include_udp: bool) -> Vec<NetRow> {
    text.lines()
        .filter_map(parse_netstat_line)
        .filter(|row| row.is_listener())
        .filter(|row| include_udp || row.proto == "TCP")
        .filter(|row| ports.contains(&row.local_port))
        .collect()
}

/// Raw `netstat -ano` text. The full table, not `-p TCP`: an IPv6-only listener — which
/// is what Vite is by default — appears only in the unfiltered output.
#[cfg(target_os = "windows")]
fn netstat_text() -> Option<String> {
    let output = hidden_command("cmd")
        .args(["/c", "netstat", "-ano"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(decode_gbk(&output.stdout))
}

#[cfg(not(target_os = "windows"))]
fn netstat_text() -> Option<String> {
    None
}

/// Every PID holding one of `ports`, in netstat order.
pub(crate) fn listener_pids(ports: &[u16]) -> Vec<(u16, u32)> {
    match netstat_text() {
        Some(text) => matching_listeners(&text, ports, false)
            .into_iter()
            .map(|row| (row.local_port, row.pid))
            .collect(),
        None => Vec::new(),
    }
}

/// The PID listening on `port`, if any. PIDs are recycled by the OS, so callers must
/// treat the answer as "who held it when we looked", not as a stable handle.
pub(crate) fn find_pid_by_port(port: u16) -> Option<u32> {
    listener_pids(&[port]).into_iter().next().map(|(_, pid)| pid)
}

/// Kill a PID (and its process tree on Windows via `taskkill /T`).
/// "Process not found" is treated as success — the target may already be gone.
pub(crate) fn kill_pid(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let output = hidden_command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output()
            .map_err(|e| format!("taskkill 无法启动: {e}"))?;
        if output.status.success() {
            // The 快捷应用 running badges read a ~800ms tasklist snapshot; drop it so a
            // kill is visible there immediately instead of one refresh late.
            invalidate_tasklist_cache();
            return Ok(());
        }
        // GBK, not lossy UTF-8: the "already gone" verdict below matches on Chinese
        // wording, which lossy decoding would replace with U+FFFD and silently break.
        let stderr = decode_gbk(&output.stderr);
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("not found")
            || stderr.contains("没有运行")
            || stderr.contains("找不到")
            || stderr.contains("不存在")
        {
            invalidate_tasklist_cache();
            return Ok(());
        }
        Err(format!(
            "taskkill PID {pid} 失败: {}",
            stderr.trim().replace(['\r', '\n'], " ")
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let output = Command::new("kill")
            .arg(pid.to_string())
            .output()
            .map_err(|e| format!("kill 无法启动: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        // ESRCH — no such process
        if output.status.code() == Some(1) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("kill PID {pid} 失败: {}", stderr.trim()))
    }
}

/// Is something bound to `port`? Loopback bind probe — cheap, but it only answers for
/// an *exact* IPv4 loopback binding: measured on this machine, binding `127.0.0.1:P`
/// still **succeeds** while another process holds `0.0.0.0:P` (Windows lets a specific
/// address coexist with a wildcard one), and it cannot see IPv6-only listeners at all
/// (Vite's default). So `false` here does not mean "free" — callers that must be sure
/// re-read netstat.
pub(crate) fn is_port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// Wait for a killed server to release its port, so its replacement can bind it.
/// Without this, the launch-time port wait can see the dying socket and treat the
/// successor as up too early, which then fails to listen.
///
/// Returns `true` when the port is free; `false` if still bound after the deadline.
pub(crate) fn wait_for_port_release(port: u16) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while is_port_in_use(port) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
    }
    !is_port_in_use(port)
}

// ── 端口查杀命令（快捷应用页的「端口查杀」面板） ──────────────────────────────

/// One process holding one queried port. Dual-stack listeners arrive as two netstat
/// rows with the same PID, so the addresses are folded together: one card, one kill.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortListener {
    pub queried_port: u16,
    pub pid: u32,
    /// `"TCP"` | `"UDP"`.
    pub proto: String,
    /// e.g. `["0.0.0.0", "[::]"]` — a `[` prefix is how the UI reads "IPv6".
    pub local_addrs: Vec<String>,
    pub state: String,
    pub process_name: String,
    /// Empty when the process is gone or access is denied.
    pub exe_path: String,
    /// `""` | `"system-pid"` | `"critical-process"`.
    pub protection: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortKillResult {
    pub port: u16,
    pub killed_pids: Vec<u32>,
    /// Per-PID Chinese detail: refused, already gone, or taskkill failed.
    pub failed: Vec<String>,
    pub still_listening: Vec<PortListener>,
    /// Only true when the bind probe *and* a fresh netstat agree the port is free.
    pub port_released: bool,
}

/// Image names this tool must never terminate. The name from a fresh `tasklist`
/// snapshot decides, not the PID — PIDs are recycled by the OS.
const CRITICAL_PROCESSES: [&str; 7] =
    ["smss", "csrss", "wininit", "services", "lsass", "winlogon", "svchost"];

/// 0 is Idle (it owns every TIME_WAIT row), 4 is System — and SMB sits on 445 under
/// PID 4, which makes "kill 445" the most likely accidental hit on a dev box.
pub(crate) fn protection_for(pid: u32, image_name: &str) -> &'static str {
    if pid <= 4 {
        return "system-pid";
    }
    let stem = crate::app_launcher_commands::normalize_image_name(image_name).to_ascii_lowercase();
    if CRITICAL_PROCESSES.contains(&stem.as_str()) {
        return "critical-process";
    }
    ""
}

/// At most this many ports per query: one `netstat` spawn covers them all, and the
/// cap keeps a pasted list from turning into a runaway loop.
const MAX_QUERIED_PORTS: usize = 64;

fn sanitize_ports(ports: Vec<u16>) -> Result<Vec<u16>, String> {
    let mut seen: Vec<u16> = Vec::new();
    for port in ports {
        if port == 0 || seen.contains(&port) {
            continue;
        }
        seen.push(port);
    }
    if seen.is_empty() {
        return Err("请输入 1-65535 的端口号".into());
    }
    if seen.len() > MAX_QUERIED_PORTS {
        return Err(format!("一次最多查询 {MAX_QUERIED_PORTS} 个端口"));
    }
    Ok(seen)
}

/// Group raw rows into one entry per (port, proto, PID) and attach process identity.
fn listeners_from_text(text: &str, ports: &[u16]) -> Vec<PortListener> {
    let mut out: Vec<PortListener> = Vec::new();
    for row in matching_listeners(text, ports, true) {
        // Position first, then mutate: holding an `iter_mut` borrow across the push in
        // the `None` arm does not borrow-check.
        let existing = out.iter().position(|l| {
            l.pid == row.pid && l.proto == row.proto && l.queried_port == row.local_port
        });
        match existing {
            Some(index) => {
                if !out[index].local_addrs.contains(&row.local_addr) {
                    out[index].local_addrs.push(row.local_addr);
                }
            }
            None => out.push(PortListener {
                queried_port: row.local_port,
                pid: row.pid,
                proto: row.proto,
                local_addrs: vec![row.local_addr],
                state: row.state,
                process_name: String::new(),
                exe_path: String::new(),
                protection: "",
            }),
        }
    }
    let names = tasklist_names();
    for listener in out.iter_mut() {
        // A PID that vanished between netstat and tasklist simply reports no name; the
        // kill path re-checks protection against a fresh snapshot anyway.
        listener.process_name = names.get(&listener.pid).cloned().unwrap_or_default();
        listener.protection = protection_for(listener.pid, &listener.process_name);
        listener.exe_path = exe_path_for_pid(listener.pid);
    }
    out
}

fn current_listeners(ports: &[u16]) -> Result<Vec<PortListener>, String> {
    let text = netstat_text().ok_or_else(|| "当前系统不支持端口查询".to_string())?;
    Ok(listeners_from_text(&text, ports))
}

#[tauri::command]
pub async fn find_port_listeners(ports: Vec<u16>) -> Result<Vec<PortListener>, String> {
    tokio::task::spawn_blocking(move || current_listeners(&sanitize_ports(ports)?))
        .await
        .map_err(|e| format!("任务执行失败: {e}"))?
}

/// Terminate the given PIDs, but only those that *still* hold `port`.
///
/// The re-check is the point: the panel has no auto-refresh, so a row the user clicked
/// minutes after the process exited — and its PID was recycled — would otherwise kill
/// an unrelated process.
#[tauri::command]
pub async fn kill_port_listeners(
    port: u16,
    pids: Vec<u32>,
) -> Result<PortKillResult, String> {
    tokio::task::spawn_blocking(move || kill_port_listeners_inner(port, pids))
        .await
        .map_err(|e| format!("任务执行失败: {e}"))?
}

fn kill_port_listeners_inner(port: u16, pids: Vec<u32>) -> Result<PortKillResult, String> {
    let ports = sanitize_ports(vec![port])?;
    if pids.is_empty() {
        return Err("没有指定要结束的进程".into());
    }
    let held = current_listeners(&ports)?;
    let names = tasklist_names();

    let mut killed = Vec::new();
    let mut failed = Vec::new();
    for pid in pids {
        if !held.iter().any(|l| l.pid == pid) {
            failed.push(format!("PID {pid} 已不再监听 {port}，未执行结束"));
            continue;
        }
        let name = names.get(&pid).cloned().unwrap_or_default();
        match protection_for(pid, &name) {
            "" => {}
            _ => {
                failed.push(format!(
                    "PID {pid}{} 是系统关键进程，已拒绝结束",
                    if name.is_empty() { String::new() } else { format!(" · {name}") }
                ));
                continue;
            }
        }
        match kill_pid(pid) {
            Ok(()) => killed.push(pid),
            Err(e) => failed.push(e),
        }
    }

    // The loopback bind probe alone would call an IPv6-only survivor "released".
    let remaining = current_listeners(&ports)?;
    let still_tcp = remaining
        .iter()
        .any(|l| l.proto == "TCP" && l.queried_port == port);
    let port_released = wait_for_port_release(port) && !still_tcp;

    Ok(PortKillResult {
        port,
        killed_pids: killed,
        failed,
        still_listening: remaining,
        port_released,
    })
}

/// PID → image name, from one `tasklist /FO CSV /NH` snapshot.
#[cfg(target_os = "windows")]
fn tasklist_names() -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(output) = hidden_command("tasklist").args(["/FO", "CSV", "/NH"]).output() else {
        return map;
    };
    if !output.status.success() {
        return map;
    }
    for line in decode_gbk(&output.stdout).lines() {
        // CSV row: "node.exe","26108","Console","1","123,456 K"
        let fields = parse_csv_fields(line);
        if let (Some(name), Some(Ok(pid))) = (fields.first(), fields.get(1).map(|v| v.parse::<u32>()))
        {
            map.insert(pid, name.clone());
        }
    }
    map
}

#[cfg(not(target_os = "windows"))]
fn tasklist_names() -> std::collections::HashMap<u32, String> {
    std::collections::HashMap::new()
}

/// Split one CSV line, honouring the quotes `tasklist` puts around every field.
fn parse_csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut current));
            }
            c if !c.is_whitespace() || in_quotes => current.push(c),
            _ => {}
        }
    }
    fields.push(current);
    fields
}

/// Full executable path — the detail that lets a user tell *their* `node.exe` from
/// some vendor's before killing it. `PROCESS_QUERY_LIMITED_INFORMATION` is enough even
/// for a 32-bit view of a 64-bit process, and needs no elevation for own-user processes.
#[cfg(target_os = "windows")]
fn exe_path_for_pid(pid: u32) -> String {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const MAX_PATH_CHARS: usize = 4096;
    let mut buffer = [0u16; MAX_PATH_CHARS];
    let mut size = MAX_PATH_CHARS as u32;
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let queried =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        match queried {
            Ok(()) => String::from_utf16_lossy(&buffer[..size as usize]),
            Err(_) => String::new(),
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn exe_path_for_pid(_pid: u32) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte-accurate to a real Chinese Windows `netstat -ano`.
    const TCP_V4: &str = "  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       26108";
    const TCP_V6_WILDCARD: &str = "  TCP    [::]:135               [::]:0                 LISTENING       1724";
    const TCP_V6_LOOPBACK: &str = "  TCP    [::1]:1420             [::]:0                 LISTENING       37180";
    const UDP_ROW: &str = "  UDP    0.0.0.0:123            *:*                                    35684";
    const ESTABLISHED: &str = "  TCP    10.16.50.30:56486      157.255.219.80:8080    ESTABLISHED     7988";
    const TIME_WAIT: &str = "  TCP    10.16.50.30:60138      122.96.99.72:8080      TIME_WAIT       0";

    #[test]
    fn parses_tcp_v4_and_v6_with_the_right_port() {
        let row = parse_netstat_line(TCP_V4).unwrap();
        assert_eq!((row.proto.as_str(), row.local_addr.as_str(), row.local_port, row.pid), ("TCP", "0.0.0.0", 8080, 26108));
        assert_eq!(row.state, "LISTENING");
        assert!(!row.local_addr.starts_with('['));

        // The port comes from after the last colon, not from inside "[::]".
        let v6 = parse_netstat_line(TCP_V6_WILDCARD).unwrap();
        assert_eq!((v6.local_addr.as_str(), v6.local_port, v6.pid), ("[::]", 135, 1724));
        assert!(v6.local_addr.starts_with('['));
        assert_eq!(parse_netstat_line(TCP_V6_LOOPBACK).unwrap().local_port, 1420);
    }

    #[test]
    fn udp_row_has_no_state_but_still_yields_a_pid() {
        let row = parse_netstat_line(UDP_ROW).unwrap();
        assert_eq!(row.proto, "UDP");
        assert_eq!(row.state, "");
        assert_eq!((row.local_port, row.pid), (123, 35684));
    }

    #[test]
    fn header_and_garbage_lines_are_not_rows() {
        assert!(parse_netstat_line("  Proto  Local Address          Foreign Address        State           PID").is_none());
        assert!(parse_netstat_line("").is_none());
        assert!(parse_netstat_line("  TCP    bad").is_none());
        // A non-numeric PID is not a row either.
        assert!(parse_netstat_line("  TCP    0.0.0.0:80    0.0.0.0:0    LISTENING    abc").is_none());
    }

    #[test]
    fn exact_port_match_does_not_hit_longer_ports() {
        let near = "  TCP    0.0.0.0:30800          0.0.0.0:0              LISTENING       9";
        let hits = matching_listeners(near, &[3080], false);
        assert!(hits.is_empty(), "3080 must not match a listener on 30800");
        assert_eq!(matching_listeners(near, &[30800], false).len(), 1);
    }

    #[test]
    fn only_listening_sockets_are_kill_targets() {
        // An ESTABLISHED row whose *remote* end is on 8080 is a client, not the server.
        assert!(matching_listeners(ESTABLISHED, &[8080], false).is_empty());
        // TIME_WAIT rows carry PID 0 and are never owned by a killable process.
        assert!(matching_listeners(TIME_WAIT, &[8080], false).is_empty());
        assert!(matching_listeners(TCP_V4, &[8080], false).len() == 1);
    }

    #[test]
    fn udp_is_hidden_from_a_tcp_only_query() {
        assert!(matching_listeners(UDP_ROW, &[123], false).is_empty());
        assert_eq!(matching_listeners(UDP_ROW, &[123], true).len(), 1);
    }

    #[test]
    fn dual_stack_listeners_are_two_rows_one_pid() {
        let text = format!("{TCP_V4}\n  TCP    [::]:8080            [::]:0                 LISTENING       26108\n");
        let hits = matching_listeners(&text, &[8080], false);
        assert_eq!(hits.len(), 2, "both families are reported…");
        let pids: Vec<u32> = hits.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![26108, 26108], "…and the caller dedupes by PID so one process is killed once");
    }

    #[test]
    fn multi_port_query_matches_either_port() {
        let text = format!("{TCP_V4}\n{TCP_V6_LOOPBACK}\n");
        let hits = matching_listeners(&text, &[8080, 1420], false);
        assert_eq!(hits.iter().map(|h| h.local_port).collect::<Vec<_>>(), vec![8080, 1420]);
        assert_eq!(matching_listeners(&text, &[9999], false).len(), 0);
    }

    #[test]
    fn dual_stack_rows_become_one_listener_with_both_addresses() {
        let text = format!("{TCP_V4}\n  TCP    [::]:8080            [::]:0                 LISTENING       26108\n");
        let listeners = listeners_from_text(&text, &[8080]);
        assert_eq!(listeners.len(), 1, "one card, one kill — not two rows for the same process");
        assert_eq!(listeners[0].local_addrs, vec!["0.0.0.0", "[::]"]);
        // No protection assertion here: `listeners_from_text` resolves the pid
        // against the LIVE process table and 26108 gets recycled by Windows
        // like any pid (it was node.exe this morning, a system process later,
        // flipping this test with no code change). The protection verdict is
        // covered by `system_pids_and_critical_images_are_protected`.
    }

    #[test]
    fn system_pids_and_critical_images_are_protected() {
        assert_eq!(protection_for(0, "System Idle Process"), "system-pid");
        // SMB on 445 really is held by PID 4, so this is the accidental-hit case.
        assert_eq!(protection_for(4, "System"), "system-pid");
        assert_eq!(protection_for(1724, "svchost.exe"), "critical-process");
        assert_eq!(protection_for(1234, "CSRSS.EXE"), "critical-process");
        assert_eq!(protection_for(26108, "node.exe"), "");
        assert_eq!(protection_for(3001, "java"), "", "a bare stem without .exe is still recognised");
        assert_eq!(protection_for(4321, "explorer.exe"), "", "recoverable, and a plausible accidental holder — warned, not blocked");
    }

    #[test]
    fn tasklist_csv_rows_map_pid_to_name_and_skip_the_header() {
        assert_eq!(
            parse_csv_fields("\"node.exe\",\"26108\",\"Console\",\"1\",\"123,456 K\""),
            vec!["node.exe", "26108", "Console", "1", "123,456 K"]
        );
        // The embedded comma inside the memory column must not shift the PID.
        let fields = parse_csv_fields("\"python.exe\",\"9\",\"Services\",\"0\",\"1,234 K\"");
        assert_eq!((fields[0].as_str(), fields[1].as_str()), ("python.exe", "9"));
    }

    /// End-to-end on the kill path, against a throwaway listener — never a real dev
    /// server. Run with `cargo test -- --ignored kills_a_real_listener`.
    #[test]
    #[ignore = "audit-only: spawns a process on a free port and terminates it"]
    fn kills_a_real_listener_and_releases_the_port() {
        use std::process::{Command, Stdio};

        let probe = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let mut child = Command::new("node")
            .args([
                "-e",
                &format!("require('net').createServer().listen({port}, '0.0.0.0')"),
            ])
            .stdout(Stdio::null())
            // A squatted port (e.g. a leftover listener from an aborted run) shows up
            // here as EADDRINUSE; without this the failure is just "never came up".
            .stderr(Stdio::piped())
            .spawn()
            .expect("node on PATH");

        // netstat, not `is_port_in_use`: the bind probe cannot see a wildcard (0.0.0.0)
        // listener from a 127.0.0.1 bind on Windows, which is exactly what node creates.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while find_pid_by_port(port).is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if find_pid_by_port(port).is_none() {
            let _ = child.kill();
            let captured = child.wait_with_output().ok();
            let why = captured
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .unwrap_or_default();
            panic!("nothing bound {port} within 10s. node said: {why}");
        }

        let found = current_listeners(&[port]).unwrap();
        assert_eq!(
            found.iter().map(|l| l.pid).collect::<Vec<_>>(),
            vec![child.id()],
            "netstat should report exactly the child we spawned"
        );
        assert_eq!(found[0].protection, "");
        assert!(!found[0].exe_path.is_empty(), "the image path is what makes a kill safe");

        let result = kill_port_listeners_inner(port, found.iter().map(|l| l.pid).collect()).unwrap();
        let _ = child.wait();

        assert!(result.failed.is_empty(), "kill reported failures: {:?}", result.failed);
        assert_eq!(result.killed_pids, vec![child.id()], "the killed PID should be the child we spawned");
        assert!(result.port_released, "port {port} should be free: {:?}", result.still_listening);
        assert!(find_pid_by_port(port).is_none(), "netstat should no longer report a holder");
    }

    #[test]
    fn finds_our_own_listener_end_to_end() {
        // The read path for real: spawn netstat, GBK-decode it, parse, then resolve this
        // process's own name and image path. Binding port 0 keeps it off fixed ports —
        // never 1420, which is the developer's own Vite.
        let bound = TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral port");
        let port = bound.local_addr().unwrap().port();
        let listeners = current_listeners(&[port]).unwrap();
        drop(bound);

        let me = listeners.iter().find(|l| l.pid == std::process::id());
        let Some(listener) = me else {
            panic!("our own listener on {port} was not found: {listeners:?}");
        };
        assert_eq!(listener.proto, "TCP");
        assert_eq!(listener.state, "LISTENING");
        assert_eq!(listener.local_addrs, vec!["127.0.0.1"]);
        assert_eq!(listener.protection, "", "the test binary is killable, not a system PID");
        assert!(
            listener.exe_path.to_ascii_lowercase().ends_with(".exe"),
            "image path should resolve for our own process, got {:?}",
            listener.exe_path
        );
    }

    #[test]
    fn port_lists_are_deduped_zero_dropped_and_capped() {
        assert_eq!(sanitize_ports(vec![8080, 8080, 0, 3000]).unwrap(), vec![8080, 3000]);
        assert!(sanitize_ports(vec![0]).is_err());
        assert!(sanitize_ports(Vec::new()).is_err());
        assert!(sanitize_ports((1..=65).collect()).is_err(), "one netstat spawn is cheap, a 65-port loop is not");
    }
}
