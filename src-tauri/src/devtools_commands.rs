//! DevTools backend: port/process listing + HTTP client.
//!
//! Windows-only (matches the rest of the app). Uses only `netstat`,
//! `tasklist` and `taskkill` (always present on Windows) plus the already
//! available `reqwest` crate for the HTTP client.

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Instant;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEntry {
    pub proto: String,
    pub local_addr: String,
    pub local_port: u16,
    pub remote_addr: String,
    pub remote_port: u16,
    pub state: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub memory: String,
    pub path: String,
    pub services: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderPair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Option<Vec<HeaderPair>>,
    pub body: Option<String>,
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub status: u16,
    pub duration_ms: u128,
    pub headers: Vec<HeaderPair>,
    pub body: String,
}

#[cfg(windows)]
fn netstat_cmd() -> Command {
    let mut cmd = Command::new("netstat");
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// List listening TCP/UDP ports with their owning PID via `netstat -ano`.
#[tauri::command]
pub fn devtools_list_ports() -> Result<Vec<PortEntry>, String> {
    #[cfg(target_os = "windows")]
    {
        let output = netstat_cmd()
            .args(["-ano"])
            .output()
            .map_err(|e| format!("执行 netstat 失败: {e}"))?;

        let text = String::from_utf8_lossy(if output.status.success() {
            &output.stdout
        } else {
            &output.stderr
        });

        let mut entries = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            let mut parts: Vec<&str> = trimmed.split_whitespace().collect();
            // UDP has no state column; normalise by inserting empty state.
            if parts.len() == 4 && parts[0].eq_ignore_ascii_case("udp") {
                parts.insert(3, "");
            }
            if parts.len() < 5 {
                continue;
            }
            let proto = parts[0].to_ascii_uppercase();
            if !proto.eq_ignore_ascii_case("tcp") && !proto.eq_ignore_ascii_case("udp") {
                continue;
            }
            let (local_addr, local_port) = match parse_addr_port(parts[1]) {
                Some(v) => v,
                None => continue,
            };
            let (remote_addr, remote_port) = parse_addr_port(parts[2]).unwrap_or_else(|| ("*".to_string(), 0));
            let state = if proto.eq_ignore_ascii_case("tcp") {
                parts[3].to_string()
            } else {
                String::new()
            };
            let pid: u32 = parts.last().and_then(|p| p.parse().ok()).unwrap_or(0);

            entries.push(PortEntry {
                proto,
                local_addr,
                local_port,
                remote_addr,
                remote_port,
                state,
                pid,
            });
        }

        entries.retain(|e| e.pid != 0 || !e.state.is_empty());
        entries.sort_by(|a, b| a.local_port.cmp(&b.local_port));
        Ok(entries)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = ();
        Err("当前系统不支持端口查看".into())
    }
}

fn parse_addr_port(s: &str) -> Option<(String, u16)> {
    // IPv4 127.0.0.1:8080 , IPv6 [::]:8080 , *:*
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (host, port_str) = if let Some(idx) = s.rfind(':') {
        (&s[..idx], &s[idx + 1..])
    } else {
        return None;
    };
    let port: u16 = port_str.parse().ok()?;
    let host = host.trim_matches(['[', ']']).to_string();
    Some((host, port))
}

/// Resolve a PID to process details: name, memory, executable path and
/// (for shared hosts like svchost) the service names it hosts.
#[tauri::command]
pub fn devtools_process_name(pid: u32) -> Result<ProcessInfo, String> {
    #[cfg(target_os = "windows")]
    {
        let mut name = String::new();
        let mut memory = String::new();

        // name + memory via tasklist (CSV, no header)
        if let Ok(output) = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let fields = parse_csv_line(line);
                if fields.len() >= 2 {
                    let n = &fields[0];
                    if !n.eq_ignore_ascii_case("image name") && !n.is_empty() {
                        name = n.clone();
                        memory = fields.get(4).cloned().unwrap_or_default();
                        break;
                    }
                }
            }
        }

        // full executable path via PowerShell (most reliable across Windows versions)
        let path = if let Ok(output) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("(Get-Process -Id {pid} -ErrorAction SilentlyContinue).Path"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if p.is_empty() || p.eq_ignore_ascii_case("null") {
                // fallback to wmic if PowerShell can't access the process
                wpc(pid)
            } else {
                p
            }
        } else {
            wpc(pid)
        };

        // service names hosted by this PID (relevant for svchost, dllhost, etc.)
        let services = if let Ok(output) = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH", "/SVC"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut svcs: Vec<String> = Vec::new();
            for line in text.lines() {
                let fields = parse_csv_line(line);
                // CSV layout with /SVC: name, pid, session, session#, mem, svc1, svc2, ...
                if fields.len() > 5 {
                    for s in &fields[5..] {
                        let s = s.trim();
                        if !s.is_empty() {
                            svcs.push(s.to_string());
                        }
                    }
                }
            }
            svcs.join(", ")
        } else {
            String::new()
        };

        Ok(ProcessInfo {
            pid,
            name,
            memory,
            path,
            services,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        Err("当前系统不支持进程查看".into())
    }
}

/// Fallback: resolve executable path via WMI (`wmic` is deprecated but still
/// present on all supported Windows versions and works for system processes
/// that PowerShell's Get-Process can't access due to permissions.
fn wpc(pid: u32) -> String {
    if let Ok(output) = Command::new("wmic")
        .args([
            "process",
            "where",
            &format!("processid={pid}"),
            "get",
            "executablepath",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.eq_ignore_ascii_case("ExecutablePath") {
                return line.to_string();
            }
        }
    }
    String::new()
}

/// Parse a single CSV line respecting quoted fields (e.g. `"a","b","c"`).
fn parse_csv_line(line: &str) -> Vec<String> {
    let line = line.trim();
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quote = !in_quote,
            ',' if !in_quote => {
                fields.push(cur.trim().trim_matches('"').to_string());
                cur = String::new();
            }
            _ => cur.push(ch),
        }
    }
    fields.push(cur.trim().trim_matches('"').to_string());
    fields
}

/// Force-kill a process by PID.
#[tauri::command]
pub fn devtools_kill_process(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("执行 taskkill 失败: {e}"))?;

        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{stdout} {stderr}");
        if combined.to_ascii_lowercase().contains("not found")
            || combined.contains("找不到")
            || combined.contains("没有")
        {
            return Err("进程已不存在".into());
        }
        Err(format!("结束进程失败: {}", combined.trim()))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        Err("当前系统不支持进程结束".into())
    }
}

/// Bulk-resolve process info for many PIDs in one shot.
///
/// Spawns just three commands total (regardless of PID count):
///   1. `tasklist /FO CSV /SVC`  → name, memory, services per PID
///   2. PowerShell `Get-Process` → executable path per PID
/// This replaces the old per-PID hover path that spawned 3 processes *each*.
#[tauri::command]
pub fn devtools_resolve_processes(pids: Vec<u32>) -> Result<Vec<ProcessInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        use std::collections::HashSet;
        let wanted: HashSet<u32> = pids.into_iter().filter(|p| *p != 0).collect();
        let mut results: Vec<ProcessInfo> = Vec::new();

        // 1) name + memory + services for all processes in one tasklist call
        let mut name_map: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();
        if let Ok(output) = Command::new("tasklist")
            .args(["/FO", "CSV", "/NH", "/SVC"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let fields = parse_csv_line(line);
                if fields.len() < 2 {
                    continue;
                }
                if let Ok(pid) = fields[1].parse::<u32>() {
                    if wanted.contains(&pid) {
                        let name = if fields[0].eq_ignore_ascii_case("image name") {
                            String::new()
                        } else {
                            fields[0].clone()
                        };
                        let memory = fields.get(4).cloned().unwrap_or_default();
                        let services = if fields.len() > 5 {
                            fields[5..]
                                .iter()
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        } else {
                            String::new()
                        };
                        name_map.insert(pid, (name, memory, services));
                    }
                }
            }
        }

        // 2) executable paths via a single PowerShell call for all wanted PIDs
        let mut path_map: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        if !wanted.is_empty() {
            // Single PowerShell call returns "pid|path" lines for all wanted PIDs
            let ps = format!(
                "$filter = @({}); Get-Process -Id $filter -ErrorAction SilentlyContinue | ForEach-Object {{ \"$($_.Id)|$($_.Path)\" }}",
                wanted.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",")
            );
            if let Ok(output) = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let line = line.trim();
                    if let Some(pos) = line.find('|') {
                        if let Ok(pid) = line[..pos].parse::<u32>() {
                            let path = line[pos + 1..].trim().to_string();
                            if !path.is_empty() && !path.eq_ignore_ascii_case("null") {
                                path_map.insert(pid, path);
                            }
                        }
                    }
                }
            }
        }

        // 3) assemble results
        for pid in &wanted {
            let (name, memory, services) = name_map
                .remove(pid)
                .unwrap_or_else(|| (String::new(), String::new(), String::new()));
            let path = path_map.remove(pid).unwrap_or_default();
            results.push(ProcessInfo {
                pid: *pid,
                name,
                memory,
                path,
                services,
            });
        }

        Ok(results)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = pids;
        Err("当前系统不支持进程查看".into())
    }
}

/// Minimal HTTP client backed by `reqwest`. Runs on a blocking thread so the
/// UI thread never stalls on slow responses.
#[tauri::command]
pub async fn devtools_http_request(req: HttpRequest) -> Result<HttpResponse, String> {
    let method = req.method.to_ascii_uppercase();
    let allowed = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    if !allowed.contains(&method.as_str()) {
        return Err(format!("不支持的 HTTP 方法: {method}"));
    }

    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        _ => return Err(format!("不支持的 HTTP 方法: {method}")),
    };

    let timeout = std::time::Duration::from_secs(req.timeout_sec.unwrap_or(30));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut builder = client.request(method, &req.url);
    if let Some(headers) = req.headers {
        for h in headers {
            if h.key.trim().is_empty() {
                continue;
            }
            builder = builder.header(h.key, h.value);
        }
    }
    if let Some(body) = req.body {
        builder = builder.body(body);
    }

    let start = Instant::now();
    let resp = builder
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let resp_headers: Vec<HeaderPair> = resp
        .headers()
        .iter()
        .map(|(k, v)| HeaderPair {
            key: k.to_string(),
            value: v.to_str().unwrap_or("").to_string(),
        })
        .collect();
    let body = resp.text().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis();

    Ok(HttpResponse {
        status,
        duration_ms,
        headers: resp_headers,
        body,
    })
}
