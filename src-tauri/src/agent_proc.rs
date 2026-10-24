//! AI hub agent commands: one-shot `run_command` (cmd.exe or PowerShell, timeout,
//! head + tail clipping) and background processes that outlive a turn
//! (`start_process` / `process_output` / `stop_process`).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agent::{cancelled_message, command_blocked};
use crate::agent_fs::arg_str;
use crate::cancellation::is_cancelled;

const CMD_CAP: usize = 8_000;
const TIMEOUT_MAX: u64 = 600;
/// Rolling output kept per background process.
const BG_KEEP: usize = 200_000;
const BG_MAX: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shell {
    Cmd,
    PowerShell,
}

impl Shell {
    pub(crate) fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "powershell" | "pwsh" | "ps" => Self::PowerShell,
            _ => Self::Cmd,
        }
    }
}

fn git_network_command(command: &str) -> bool {
    let lower = command.to_lowercase();
    ["git push", "git pull", "git fetch", "git ls-remote", "git clone"].iter().any(|k| lower.contains(k))
}

fn default_timeout(command: &str) -> u64 {
    if git_network_command(command) {
        600
    } else {
        60
    }
}

/// UTF-8 when valid, else the Chinese console code page (GBK): cmd built-ins and many
/// Windows tools print in the OEM code page, which `from_utf8_lossy` turns into `�`.
pub(crate) fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

/// Keep the start and the end of long output (errors usually sit at the end).
pub(crate) fn clip_head_tail(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let head = max * 3 / 5;
    let tail = max - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(total - tail).collect();
    format!("{start}\n…（中间省略 {} 字）…\n{end}", total - head - tail)
}

/// PowerShell gets the script base64-encoded (UTF-16LE): no quoting rules to get wrong.
pub(crate) fn powershell_encoded(command: &str) -> String {
    use base64::Engine;
    let script = format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {command}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

fn build(shell: Shell, command: &str, cwd: &Path, hide_window: bool) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut c = match shell {
            Shell::Cmd => {
                let mut c = Command::new("cmd");
                // 必须 raw_arg：std 按 MSVC 规则把 `"` 转义成 `\"`，cmd.exe 不认，带引号的参数
                // （commit -m "a b"）会被拆碎。/S 让 cmd 只剥最外层这对引号，其余原样执行。
                c.raw_arg(format!("/S /C \"{command}\""));
                c
            }
            Shell::PowerShell => {
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]);
                c.arg(powershell_encoded(command));
                c
            }
        };
        c.current_dir(cwd);
        if hide_window {
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }
    #[cfg(not(windows))]
    {
        let _ = (shell, hide_window);
        let mut c = Command::new("sh");
        c.args(["-c", command]).current_dir(cwd);
        c
    }
}

fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run one command to completion in `cwd`. Output starts with `exit N` when it failed.
pub(crate) fn run_command(cwd: &Path, args: &Value, request_id: &str) -> Result<String, String> {
    let command = arg_str(args, "command");
    if command.is_empty() {
        return Err("命令为空".into());
    }
    if command_blocked(&command) {
        return Err("这条命令被拒绝".into());
    }
    let shell = Shell::parse(&arg_str(args, "shell"));
    let limit = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| default_timeout(&command))
        .clamp(1, TIMEOUT_MAX);
    // A credential dialog needs a real window. Hiding it makes push sit until the timeout.
    let mut cmd = build(shell, &command, cwd, !git_network_command(&command));
    // Commit-page push injects the Windows system proxy; a bare git push here would not.
    if git_network_command(&command) {
        if let Some((http_proxy, https_proxy)) = crate::git_commands::git_proxy_env(&cwd.to_string_lossy()) {
            cmd.env("http_proxy", &http_proxy);
            cmd.env("https_proxy", &https_proxy);
        }
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let read_all = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_handle = read_all(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_handle = read_all(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if is_cancelled(request_id) {
                    kill_tree(&mut child);
                    let _ = (out_handle.join(), err_handle.join());
                    return Err(cancelled_message());
                }
                if started.elapsed() > Duration::from_secs(limit) {
                    kill_tree(&mut child);
                    let _ = (out_handle.join(), err_handle.join());
                    return Err(format!(
                        "命令超过 {limit} 秒，已停止。长时间运行的服务请改用 start_process。"
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut text = decode_output(&out_handle.join().unwrap_or_default());
    let err = decode_output(&err_handle.join().unwrap_or_default());
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if text.trim().is_empty() {
        text = format!("exit {code}");
    } else if code != 0 {
        // 失败要说出来：只看报错文本，弱模型分不清是警告还是命令没成功。放开头，截断截不掉
        text = format!("exit {code}\n{text}");
    }
    Ok(clip_head_tail(&text, CMD_CAP))
}

struct BgProc {
    child: Child,
    command: String,
    output: Arc<Mutex<String>>,
    started: Instant,
}

static PROCS: Mutex<Option<(u32, HashMap<u32, BgProc>)>> = Mutex::new(None);

fn with_procs<T>(f: impl FnOnce(&mut u32, &mut HashMap<u32, BgProc>) -> T) -> T {
    let mut guard = PROCS.lock().expect("agent procs");
    let (next, map) = guard.get_or_insert_with(|| (1, HashMap::new()));
    f(next, map)
}

fn pump(pipe: impl Read + Send + 'static, sink: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let piece = decode_output(&buf[..n]);
                    let mut out = sink.lock().expect("bg output");
                    out.push_str(&piece);
                    if out.len() > BG_KEEP {
                        let cut = out.len() - BG_KEEP;
                        let cut = (cut..out.len()).find(|i| out.is_char_boundary(*i)).unwrap_or(cut);
                        out.drain(..cut);
                    }
                }
            }
        }
    });
}

/// 只给测试用：钉住「后台进程 #N 已启动…」这条消息格式，代理自己走 [`start_process_id`]。
#[cfg(test)]
pub(crate) fn start_process(cwd: &Path, args: &Value) -> Result<String, String> {
    start_process_id(cwd, args).map(|(_, message)| message)
}

/// Same as [`start_process`], but hands the id back separately. The agent needs it: when a
/// turn is cancelled while this call is still inside `spawn_blocking` (which cannot be
/// aborted), the child is already registered, so the caller has to stop it by id or it
/// outlives the turn with nobody knowing its number.
pub(crate) fn start_process_id(cwd: &Path, args: &Value) -> Result<(u32, String), String> {
    let command = arg_str(args, "command");
    if command.is_empty() {
        return Err("命令为空".into());
    }
    if command_blocked(&command) {
        return Err("这条命令被拒绝".into());
    }
    let running = with_procs(|_, map| map.values_mut().map(|p| matches!(p.child.try_wait(), Ok(None))).filter(|alive| *alive).count());
    if running >= BG_MAX {
        return Err(format!("后台进程已有 {BG_MAX} 个在运行，先用 stop_process 停掉不用的"));
    }
    let shell = Shell::parse(&arg_str(args, "shell"));
    let mut cmd = build(shell, &command, cwd, true);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let output = Arc::new(Mutex::new(String::new()));
    if let Some(p) = child.stdout.take() {
        pump(p, output.clone());
    }
    if let Some(p) = child.stderr.take() {
        pump(p, output.clone());
    }
    let pid = child.id();
    let id = with_procs(|next, map| {
        let id = *next;
        *next += 1;
        map.insert(id, BgProc { child, command: command.clone(), output, started: Instant::now() });
        id
    });
    // Give it a moment so an immediate failure (bad command) shows up in the first read.
    std::thread::sleep(Duration::from_millis(800));
    let first = process_output(&serde_json::json!({ "id": id }))?;
    Ok((id, format!("后台进程 #{id} 已启动（pid {pid}）：{command}\n{first}")))
}

fn proc_id(args: &Value) -> Result<u32, String> {
    args.get("id")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok())))
        .map(|n| n as u32)
        .ok_or_else(|| "缺少进程编号 id".into())
}

pub(crate) fn process_output(args: &Value) -> Result<String, String> {
    let id = proc_id(args)?;
    let tail = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(CMD_CAP as u64).clamp(200, 20_000) as usize;
    with_procs(|_, map| {
        let Some(p) = map.get_mut(&id) else {
            return Err(format!("没有编号 #{id} 的后台进程"));
        };
        let status = match p.child.try_wait() {
            Ok(Some(s)) => format!("已退出（exit {}）", s.code().unwrap_or(-1)),
            Ok(None) => format!("运行中，已 {} 秒", p.started.elapsed().as_secs()),
            Err(e) => format!("状态未知：{e}"),
        };
        let out = p.output.lock().expect("bg output");
        let total = out.chars().count();
        let shown: String = out.chars().skip(total.saturating_sub(tail)).collect();
        Ok(format!("#{id} {status}：{}\n{}", p.command, if shown.is_empty() { "（暂无输出）".into() } else { shown }))
    })
}

pub(crate) fn stop_process(args: &Value) -> Result<String, String> {
    let id = proc_id(args)?;
    let mut p = with_procs(|_, map| map.remove(&id)).ok_or_else(|| format!("没有编号 #{id} 的后台进程"))?;
    kill_tree(&mut p.child);
    Ok(format!("已停止后台进程 #{id}：{}", p.command))
}

/// App exit: no dev server started by the agent outlives the workbench.
pub(crate) fn kill_all() {
    let procs: Vec<BgProc> = with_procs(|_, map| map.drain().map(|(_, p)| p).collect());
    for mut p in procs {
        kill_tree(&mut p.child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_parse_and_head_tail_clip() {
        assert_eq!(Shell::parse("PowerShell"), Shell::PowerShell);
        assert_eq!(Shell::parse(""), Shell::Cmd);
        assert_eq!(clip_head_tail("short", 10), "short");
        let long: String = "a".repeat(50) + &"z".repeat(50);
        let clipped = clip_head_tail(&long, 20);
        assert!(clipped.starts_with(&"a".repeat(12)) && clipped.ends_with(&"z".repeat(8)));
        assert!(clipped.contains("省略 80 字"));
    }

    #[test]
    fn output_falls_back_to_gbk() {
        assert_eq!(decode_output("中文".as_bytes()), "中文");
        let (gbk, _, _) = encoding_rs::GBK.encode("中文");
        assert_eq!(decode_output(&gbk), "中文");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_and_cmd_run_in_cwd() {
        let dir = std::env::temp_dir();
        let out = run_command(&dir, &json!({ "command": "echo \"a b\"" }), "t-proc").unwrap();
        assert!(out.contains("\"a b\""), "{out}");
        let ps = run_command(&dir, &json!({ "command": "Write-Output 'x y'; Write-Output \"中\"", "shell": "powershell" }), "t-proc").unwrap();
        assert!(ps.contains("x y") && ps.contains("中"), "{ps}");
        let fail = run_command(&dir, &json!({ "command": "exit 3" }), "t-proc").unwrap();
        assert!(fail.starts_with("exit 3"), "{fail}");
        let slow = run_command(&dir, &json!({ "command": "ping -n 5 127.0.0.1", "timeout_secs": 1 }), "t-proc");
        assert!(slow.unwrap_err().contains("start_process"));
    }

    #[cfg(windows)]
    #[test]
    fn background_process_starts_reads_and_stops() {
        let dir = std::env::temp_dir();
        let started = start_process(&dir, &json!({ "command": "ping -n 30 127.0.0.1" })).unwrap();
        let id: u32 = started.split('#').nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let out = process_output(&json!({ "id": id })).unwrap();
        assert!(out.contains("运行中") && out.contains("127.0.0.1"), "{out}");
        assert!(stop_process(&json!({ "id": format!("#{id}") })).unwrap().contains("已停止"));
        assert!(process_output(&json!({ "id": id })).is_err());
    }
}
