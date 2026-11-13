//! In-memory task progress, keyed by the caller's exact SessionKey::as_str().
//! No logs, model text, tool arguments, network or disk access. Labels are static.
//! The orchestrator owns lifecycle and a separate timer calling `heartbeat`.
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CAPACITY: usize = 256;
const TTL: Duration = Duration::from_secs(2 * 60 * 60);
const HEARTBEAT: Duration = Duration::from_secs(30);
/// 工具/思考有进展时，允许提前刷新气泡的最短间隔（短于 HEARTBEAT）。
const BUBBLE_NUDGE: Duration = Duration::from_secs(20);
/// 超过此时长无新 activity：气泡标明未断线，避免被当成掉线。
const BUBBLE_IDLE: Duration = Duration::from_secs(90);
const MAX_KEY_BYTES: usize = 4096;
const MAX_HINT_CHARS: usize = 40;
const NO_TASK: &str = "当前会话暂无任务记录。";

/// 企微早 ack 占位：只用于回答开始前的第一帧；后续轮换气泡不再携带它，
/// 避免「收到，正在处理… 整理结论 · 已用时 4分17秒」这类过时且重复的拼接。
pub const ACK_PLACEHOLDER: &str = "收到，正在处理…";

/// 模型思考中的固定 hint：气泡把它当作活动状态本身，不再叠加轮换动词。
const THINKING_HINT: &str = "正在思考";

/// 轮换动词统一「动作短语」风格，不用「正在…」开头——中间段可能是
/// 「正在思考」这类自带进行时的 hint，叠在一起会读成两个并列动作。
const PULSE_VERBS: &[&str] = &[
    "持续处理",
    "深入分析",
    "核对线索",
    "关联排查",
    "汇总要点",
    "整理结论",
    "补充细节",
    "再核一遍",
    "收尾确认",
    "即将给出答复",
];

/// 上游工具 title 常是英文工具名（read / grep / bash…），企微受众统一映射为
/// 静态中文术语；未识别的 title 原样保留（可能本就是描述性文本）。
fn tool_label(head: &str) -> Option<&'static str> {
    Some(match head {
        "read" | "read_file" | "read_document_file" => "读文件",
        "write" | "write_file" => "写文件",
        "edit" | "edit_file" | "patch" => "改文件",
        "grep" | "search" | "search_content" => "搜索内容",
        "glob" | "find" | "ls" | "list" => "找文件",
        "bash" | "shell" | "command" | "run_command" | "terminal" => "跑命令",
        "web" | "web_fetch" | "fetch" | "browser" => "查网页",
        "symbols" => "查符号",
        "task" => "子任务",
        "plan" | "update_plan" | "todo" | "todowrite" => "更新计划",
        "skywalking_query" | "skywalking" => "查日志",
        "mcp" => "外部工具",
        _ => return None,
    })
}

/// 把已清洗的 hint 翻成展示术语：整体命中或首词命中皆可，其余部分原样保留。
/// MCP 工具名（`mcp__server__tool` / `mcp_server_tool`）只展示工具用途，
/// 不把协议名丢给企微用户——否则会出现 `mcp__skywalking__skywalking_query`
/// 这类又长又重复的中段。
fn display_hint(cleaned: &str) -> String {
    if let Some(label) = tool_label(cleaned) {
        return label.to_owned();
    }
    if let Some(rest) = cleaned.strip_prefix("mcp__") {
        // DSH 形态：`mcp__<server>__<tool>`，后面可能跟描述。
        let name = rest.split_whitespace().next().unwrap_or(rest);
        let tool = name.split_once("__").map_or(name, |(_, tool)| tool);
        if let Some(label) = tool_label(tool) {
            return label.to_owned();
        }
        return "外部工具".to_owned();
    }
    if cleaned.starts_with("mcp_") {
        // 宿主侧天玑的 `mcp_<server>_<tool>`：下划线无法可靠拆分服务名，
        // 只认已知工具名后缀，其余统一兜底。
        if cleaned.contains("skywalking_query") {
            return "查日志".to_owned();
        }
        return "外部工具".to_owned();
    }
    match cleaned.split_once(' ') {
        Some((head, rest)) => match tool_label(head) {
            Some(label) => format!("{label} {rest}"),
            None => cleaned.to_owned(),
        },
        None => cleaned.to_owned(),
    }
}

fn normalize_request(request: &str) -> String {
    match request.strip_prefix("wecom-preflight-") {
        Some(id) => format!("wecom-{id}"),
        None => request.to_owned(),
    }
}

fn phase_label(phase: &str) -> &'static str {
    match phase {
        "queued" => "排队等待",
        "acquisition" => "采集资料",
        "planning" | "planning_initial" | "planning_followup" => "制定查询计划（日志定点核对与补查）",
        "evidence_expand" => "展开已采集证据",
        "render_verified_answer" => "整理核验报告",
        "planning_repair" => "修正查询计划",
        "supplemental" => "补充采集",
        "final_model" => "生成最终答复",
        "engine_ready" => "准备引擎",
        "session_setup" => "准备会话",
        "model_selection" => "配置模型",
        "approval" | "waiting_approval" => "等待确认",
        "running" | "model" => "执行任务",
        "report" | "report_validation" => "整理核验报告",
        _ => "处理中",
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "completed" => "已完成",
        "cancelled" => "已取消",
        "timeout" => "已超时",
        "failed" => "已失败",
        "no_evidence" => "未取得证据",
        "busy" => "繁忙未执行",
        _ => "已结束",
    }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "user_cancelled" => "用户取消",
        "channel_changed" => "通道已变更",
        "timeout" => "任务超时",
        "failed" => "执行失败",
        "completed" => "正常完成",
        "no_evidence" => "未取得证据",
        "busy" => "任务繁忙",
        _ => "结果未记录（unknown）",
    }
}

#[derive(Debug)]
struct Final {
    at: Duration,
    status: &'static str,
    reason: &'static str,
}

#[derive(Debug)]
struct Task {
    session: String,
    request: String,
    started: Duration,
    last_activity: Duration,
    last_visible: Duration,
    body_started: bool,
    phase: &'static str,
    /// Short static activity hint for WeCom pulse (tool title / generic thought).
    hint: String,
    collected: usize,
    /// True only after `collected()` ran (log acquisition path), even when count is 0.
    log_acquisition: bool,
    terminal: Option<Final>,
}

fn sanitize_hint(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() || c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = flat.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, ch) in trimmed.chars().enumerate() {
        if i >= MAX_HINT_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}秒")
    } else {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{m}分")
        } else {
            format!("{m}分{s}秒")
        }
    }
}

fn pulse_verb(elapsed_secs: u64) -> &'static str {
    let idx = (elapsed_secs / HEARTBEAT.as_secs()) as usize % PULSE_VERBS.len();
    PULSE_VERBS[idx]
}

impl Task {
    fn expired(&self, now: Duration) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|end| now.saturating_sub(end.at) >= TTL)
    }

    /// 企微进行中气泡：单行（stream 换行易导致客户端不显示气泡）。
    /// 以轮换动词开头（它本身就带进行感）；长时间无新 activity 时标明未断线。
    /// 不再携带 ACK_PLACEHOLDER：那是开始前第一帧的职责，挂在这里会越久越怪。
    fn bubble_line(&self, now: Duration) -> String {
        let elapsed = now.saturating_sub(self.started).as_secs();
        let idle = now.saturating_sub(self.last_activity).as_secs();
        let verb = pulse_verb(elapsed);
        let base = if self.hint.is_empty() {
            self.phase
        } else {
            self.hint.as_str()
        };
        let middle = if idle >= BUBBLE_IDLE.as_secs() {
            format!(
                "{base}（已等待{}，未断线；可发停止）",
                format_elapsed(idle)
            )
        } else {
            base.to_string()
        };
        // 思考本身就是活动：不叠轮换动词，避免「正在梳理 · 正在思考」式并列。
        if self.hint == THINKING_HINT {
            return format!("{middle} · 已用时 {}…", format_elapsed(elapsed));
        }
        // 单行：动词 + 进度；勿插入 \n（企微未结束 stream 对换行不友好）。
        format!("{verb} · {middle} · 已用时 {}…", format_elapsed(elapsed))
    }

    /// 聊天窗口里的进度气泡：分行排版，主行给状态，次行给耗时与采集量。
    /// 只输出静态标签与计数，任何标签都必须先经 `phase_label` / `status_label` / `reason_label` 归一。
    /// 「已采集」只在走过日志采集（调用过 `collected`）时出现，避免讲解类问答把 0 读成查空。
    fn summary(&self, now: Duration) -> String {
        let end = self.terminal.as_ref().map_or(now, |end| end.at);
        let elapsed = end.saturating_sub(self.started).as_secs();
        let idle = now.saturating_sub(self.last_activity).as_secs();
        let collected_bit = if self.log_acquisition {
            format!("已采集：{}条", self.collected)
        } else {
            String::new()
        };
        match &self.terminal {
            Some(end) => {
                let mut lines = vec![
                    format!("✅ 最近任务：{} · {}", end.status, end.reason),
                    format!(
                        "最后阶段：{} · 耗时：{}秒 · 结束于{}秒前",
                        self.phase,
                        elapsed,
                        now.saturating_sub(end.at).as_secs()
                    ),
                ];
                if !collected_bit.is_empty() {
                    lines.push(collected_bit);
                }
                lines.join("\n")
            }
            None => {
                let icon = if self.phase == phase_label("queued") {
                    "⏳"
                } else {
                    "⚙️"
                };
                let status = if self.phase == phase_label("queued") {
                    "排队中"
                } else {
                    "进行中"
                };
                let mut second = format!("已用时：{}秒 · 最近活动距今：{}秒", elapsed, idle);
                if !collected_bit.is_empty() {
                    second.push_str(" · ");
                    second.push_str(&collected_bit);
                }
                format!("{icon} 当前任务：{status} · 阶段：{}\n{second}", self.phase)
            }
        }
    }
}

/// Entries stay in begin order. Finishing an old request never reorders them.
/// Bounded linear lookups avoid a second, potentially stale session/request index.
#[derive(Default)]
struct State {
    tasks: Vec<Task>,
    observed: Duration,
}

impl State {
    fn time(&mut self, now: Duration) -> Duration {
        self.observed = self.observed.max(now);
        self.observed
    }

    fn prune(&mut self, now: Duration) {
        self.tasks.retain(|task| !task.expired(now));
    }

    fn begin(&mut self, session: &str, request: &str, now: Duration) -> bool {
        if session.is_empty()
            || request.is_empty()
            || session.len() > MAX_KEY_BYTES
            || request.len() > MAX_KEY_BYTES
        {
            return false;
        }
        let now = self.time(now);
        self.prune(now);
        let request = normalize_request(request);
        // First begin and first final are immutable while this record is retained.
        // A request ID must not be rebound to a different session.
        if self.tasks.iter().any(|task| task.request == request) {
            return false;
        }
        if self.tasks.len() >= CAPACITY {
            let oldest = self
                .tasks
                .iter()
                .enumerate()
                .filter_map(|(index, task)| task.terminal.as_ref().map(|end| (index, end.at)))
                .min_by_key(|(_, at)| *at)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.tasks.remove(index);
                }
                None => return false, // Never evict active work, even past TTL.
            }
        }
        self.tasks.push(Task {
            session: session.to_owned(),
            request,
            started: now,
            last_activity: now,
            last_visible: now,
            body_started: false,
            phase: phase_label("queued"),
            hint: String::new(),
            collected: 0,
            log_acquisition: false,
            terminal: None,
        });
        true
    }

    fn active_mut(&mut self, request: &str) -> Option<&mut Task> {
        if request.len() > MAX_KEY_BYTES {
            return None;
        }
        let request = normalize_request(request);
        self.tasks
            .iter_mut()
            .find(|task| task.request == request && task.terminal.is_none())
    }

    fn phase(&mut self, request: &str, phase: &str, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            task.phase = phase_label(phase);
            task.last_activity = now;
        }
    }

    fn activity(&mut self, request: &str, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            task.last_activity = now;
        }
    }

    fn hint(&mut self, request: &str, text: &str, now: Duration) {
        let now = self.time(now);
        let cleaned = sanitize_hint(text);
        if cleaned.is_empty() {
            return;
        }
        if let Some(task) = self.active_mut(request) {
            task.hint = display_hint(&cleaned);
            task.last_activity = now;
        }
    }

    fn collected(&mut self, request: &str, count: usize, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            // The caller supplies a cumulative snapshot, not an increment.
            // Mark log acquisition even when count is 0 so summary can show「已采集：0条」.
            task.log_acquisition = true;
            task.collected = task.collected.max(count);
            task.last_activity = now;
        }
    }

    fn finish(&mut self, request: &str, status: &str, reason: &str, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            task.terminal = Some(Final {
                at: now,
                status: status_label(status),
                reason: reason_label(reason),
            });
        }
        self.prune(now);
    }

    fn status_reply(&self, session: &str, now: Duration) -> String {
        let now = now.max(self.observed);
        // Running work wins over a newer queued request. Within each group newest
        // begin wins; a late old final never reorders a newer task's final.
        let running = self.tasks.iter().rev().find(|task| {
            task.session == session
                && task.terminal.is_none()
                && task.phase != phase_label("queued")
        });
        let current = running.or_else(|| {
            self.tasks
                .iter()
                .rev()
                .find(|task| task.session == session && task.terminal.is_none())
        });
        let recent = || {
            self.tasks
                .iter()
                .rev()
                .find(|task| task.session == session && !task.expired(now))
        };
        let Some(task) = current.or_else(recent) else {
            return NO_TASK.to_owned();
        };
        let mut summary = task.summary(now);
        let waiting = self
            .tasks
            .iter()
            .filter(|other| {
                other.session == session
                    && other.request != task.request
                    && other.terminal.is_none()
                    && other.phase == phase_label("queued")
            })
            .count();
        if waiting > 0 {
            summary.push_str(&format!("\n⏳ 同会话另有{}条任务排队等待。", waiting));
        }
        summary
    }

    fn visible_output(&mut self, request: &str, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            task.body_started = true;
            task.last_activity = now;
            task.last_visible = now;
        }
    }

    fn heartbeat(&mut self, request: &str, now: Duration) -> Option<String> {
        let now = self.time(now);
        let task = self.active_mut(request)?;
        if task.body_started || now.saturating_sub(task.last_visible) < HEARTBEAT {
            return None;
        }
        task.last_visible = now;
        Some(task.summary(now))
    }

    /// 企微进行中短心跳：30s 节奏，正文开始后抑制。
    fn bubble_pulse(&mut self, request: &str, now: Duration) -> Option<String> {
        let now = self.time(now);
        let task = self.active_mut(request)?;
        if task.body_started || now.saturating_sub(task.last_visible) < HEARTBEAT {
            return None;
        }
        task.last_visible = now;
        Some(task.bubble_line(now))
    }

    /// 工具/思考有进展时提前刷气泡：≥20s 且尚未出正文。
    fn bubble_nudge(&mut self, request: &str, now: Duration) -> Option<String> {
        let now = self.time(now);
        let task = self.active_mut(request)?;
        if task.body_started || now.saturating_sub(task.last_visible) < BUBBLE_NUDGE {
            return None;
        }
        task.last_visible = now;
        Some(task.bubble_line(now))
    }
}

struct Runtime {
    epoch: Instant,
    state: State,
}

static PROGRESS: OnceLock<Mutex<Runtime>> = OnceLock::new();

fn with_state<T>(f: impl FnOnce(&mut State, Duration) -> T) -> T {
    let mutex = PROGRESS.get_or_init(|| {
        Mutex::new(Runtime {
            epoch: Instant::now(),
            state: State::default(),
        })
    });
    // No user code, IO or callbacks run under this lock. Keep retained state on poison.
    let mut runtime = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = runtime.epoch.elapsed();
    f(&mut runtime.state, now)
}

/// Register immediately before queue admission using the exact resolved session key.
/// False means duplicate/invalid identifier or capacity full with only active tasks.
/// The caller must use unique request IDs; deduplication ends when history is reclaimed.
pub fn begin(session: &str, request: &str) -> bool {
    with_state(|state, now| state.begin(session, request, now))
}

/// Accepts a stable phase code only; unrecognized text becomes a static generic label.
pub fn phase(request: &str, phase: &str) {
    with_state(|state, now| state.phase(request, phase, now));
}

/// Record an event without retaining any tool/model payload or resetting heartbeat.
pub fn activity(request: &str) {
    with_state(|state, now| state.activity(request, now));
}

/// Store a short static hint for WeCom pulse (tool title / generic thought). Truncated.
pub fn hint(request: &str, text: &str) {
    with_state(|state, now| state.hint(request, text, now));
}

/// Record a cumulative collected-item count; stale lower snapshots do not reduce it.
pub fn collected(request: &str, count: usize) {
    with_state(|state, now| state.collected(request, count, now));
}

/// First final wins. Later phase/activity/count/final updates are ignored.
/// Reason codes: user_cancelled/channel_changed/timeout/failed/completed/no_evidence/busy.
pub fn finish(request: &str, status: &str, reason: &str) {
    with_state(|state, now| state.finish(request, status, reason, now));
}

/// Pure state observation: does not register work, renew TTL or reset heartbeat.
pub fn status_reply(session: &str) -> String {
    with_state(|state, now| state.status_reply(session, now))
}

/// Deliberately exact short commands. `任务状态` belongs to shared-task management.
pub fn is_status_command(text: &str) -> bool {
    let text = text.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '?' | '？'
                    | '!'
                    | '！'
                    | '.'
                    | '。'
                    | ','
                    | '，'
                    | ';'
                    | '；'
                    | ':'
                    | '：'
                    | '、'
                    | '…'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
                    | '"'
                    | '\''
            )
    });
    matches!(
        text,
        "进度" | "查到哪了" | "还没好吗" | "还没好" | "好了吗" | "当前进度"
    )
}

/// Mark only ordinary answer chunks that have actually become displayable.
/// Planning/protected output must not call this. No body text is accepted or stored.
/// Once started, unsolicited heartbeat text stays suppressed for this request.
pub fn visible_output(request: &str) {
    with_state(|state, now| state.visible_output(request, now));
}

/// Poll from an independent timer. Only active tasks without visible answer text emit,
/// at most once every 30s. Tool activity and status inquiries never postpone this cadence.
/// No IO is performed. Long multi-line copy for internal /「进度」查询；企微气泡用 `bubble_pulse`。
pub fn heartbeat(request: &str) -> Option<String> {
    with_state(|state, now| state.heartbeat(request, now))
}

/// WeCom in-progress short line (verb rotation + phase/hint + elapsed). Same 30s gate as heartbeat.
pub fn bubble_pulse(request: &str) -> Option<String> {
    with_state(|state, now| state.bubble_pulse(request, now))
}

/// Early WeCom bubble refresh when tools/thoughts advance (≥20s since last visible).
pub fn bubble_nudge(request: &str) -> Option<String> {
    with_state(|state, now| state.bubble_nudge(request, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn status_commands_are_whole_short_phrases_not_shared_management() {
        for command in [
            "进度",
            "查到哪了",
            "还没好吗",
            "还没好",
            "好了吗",
            "当前进度",
        ] {
            assert!(is_status_command(command));
            assert!(is_status_command(&format!(" \n！？{command}？。 \t")));
        }
        for text in [
            "",
            "任务状态",
            "任务状态？",
            "查到哪了然后继续查",
            "还没好吗，继续",
            "进 度",
            "帮我查询当前进度",
            "/进度",
            "引用：还没好吗",
        ] {
            assert!(!is_status_command(text), "{text}");
        }
    }

    #[test]
    fn begin_is_queued_and_normalized_across_every_update() {
        let mut state = State::default();
        assert!(state.begin("exact/session", "wecom-preflight-42", at(0)));
        assert!(state
            .status_reply("exact/session", at(2))
            .contains("排队中"));
        assert!(!state.begin("other", "wecom-42", at(3)));
        state.phase("wecom-42", "acquisition", at(4));
        state.activity("wecom-preflight-42", at(5));
        state.collected("wecom-42", 12, at(6));
        state.collected("wecom-preflight-42", 2, at(7));
        let reply = state.status_reply("exact/session", at(10));
        assert!(reply.contains("采集资料") && reply.contains("已用时：10秒"));
        assert!(reply.contains("最近活动距今：3秒") && reply.contains("已采集：12条"));
        assert_eq!(state.status_reply("session", at(10)), NO_TASK);
        assert!(state.heartbeat("wecom-preflight-42", at(30)).is_some());
        state.finish("wecom-42", "cancelled", "user_cancelled", at(31));
        assert!(state
            .status_reply("exact/session", at(40))
            .contains("用户取消"));
        assert!(state.heartbeat("wecom-preflight-42", at(100)).is_none());
    }

    #[test]
    fn tool_events_every_ten_seconds_do_not_starve_thirty_second_heartbeat() {
        let mut state = State::default();
        state.begin("s", "r", at(0));
        for seconds in (10..=120).step_by(10) {
            state.activity("r", at(seconds));
            state.phase("r", "planning", at(seconds));
            state.collected("r", seconds as usize, at(seconds));
            let _ = state.status_reply("s", at(seconds));
            assert_eq!(
                state.heartbeat("r", at(seconds)).is_some(),
                seconds % 30 == 0
            );
            assert!(state.heartbeat("r", at(seconds)).is_none());
        }
        assert_eq!(state.tasks[0].last_activity, at(120));
    }

    #[test]
    fn bubble_pulse_rotates_verb_and_prefers_hint() {
        let mut state = State::default();
        state.begin("s", "wecom-preflight-9", at(0));
        state.phase("wecom-9", "planning", at(1));
        let first = state.bubble_pulse("wecom-9", at(30)).expect("pulse");
        assert!(first.starts_with("深入分析"), "{first}");
        assert!(!first.contains(ACK_PLACEHOLDER), "{first}");
        assert!(first.contains("制定查询计划"), "{first}");
        assert!(first.contains("已用时 30秒…"), "{first}");
        assert!(state.bubble_pulse("wecom-9", at(40)).is_none());
        state.hint(
            "wecom-9",
            "读日志\npath=/secret/foo",
            at(45),
        );
        let nudged = state.bubble_nudge("wecom-9", at(55)).expect("nudge");
        assert!(nudged.starts_with("深入分析"), "{nudged}");
        assert!(!nudged.contains(ACK_PLACEHOLDER), "{nudged}");
        assert!(!nudged.contains('\n'), "{nudged}");
        assert!(nudged.contains("读日志"), "{nudged}");
        assert!(!nudged.contains("制定查询计划"), "{nudged}");
        assert!(nudged.ends_with('…'), "{nudged}");
        let later = state.bubble_pulse("wecom-9", at(85)).expect("later pulse");
        assert!(later.contains("核对线索"), "{later}");
        assert!(later.contains("已用时 1分25秒…"), "{later}");
        assert!(!later.contains("未断线"), "{later}");
        state.visible_output("wecom-9", at(86));
        assert!(state.bubble_pulse("wecom-9", at(200)).is_none());
        assert!(state.bubble_nudge("wecom-9", at(220)).is_none());
    }

    #[test]
    fn bubble_line_marks_alive_when_idle_over_ninety_seconds() {
        let mut state = State::default();
        state.begin("s", "wecom-preflight-g", at(0));
        state.hint("wecom-g", "grep", at(10));
        let early = state.bubble_pulse("wecom-g", at(30)).expect("early");
        assert!(early.contains("搜索内容"), "{early}");
        assert!(!early.contains("grep"), "{early}");
        assert!(!early.contains("未断线"), "{early}");
        let idle = state.bubble_pulse("wecom-g", at(120)).expect("idle");
        assert!(idle.contains("未断线"), "{idle}");
        assert!(idle.contains("停止"), "{idle}");
        assert!(idle.contains("已等待"), "{idle}");
        assert!(idle.contains("搜索内容"), "{idle}");
    }

    /// 术语优化：思考 hint 不叠轮换动词；英文工具名映射为中文术语。
    #[test]
    fn thinking_hint_skips_verb_and_tool_titles_translate() {
        assert_eq!(display_hint("read"), "读文件");
        assert_eq!(display_hint("grep src/lib.rs"), "搜索内容 src/lib.rs");
        assert_eq!(display_hint("读日志 path=/secret"), "读日志 path=/secret");
        let mut state = State::default();
        state.begin("s", "wecom-preflight-t", at(0));
        state.hint("wecom-t", THINKING_HINT, at(1));
        let pulse = state.bubble_pulse("wecom-t", at(30)).expect("pulse");
        assert!(pulse.starts_with(THINKING_HINT), "{pulse}");
        assert!(!PULSE_VERBS.iter().any(|v| pulse.contains(v)), "{pulse}");
        assert!(pulse.contains("已用时 30秒…"), "{pulse}");
    }

    /// MCP 工具名只展示用途，不暴露协议名；未知工具兜底「外部工具」。
    #[test]
    fn mcp_tool_names_use_short_labels() {
        assert_eq!(display_hint("mcp__skywalking__skywalking_query"), "查日志");
        assert_eq!(
            display_hint("mcp__skywalking__skywalking_query 查最近 4 小时日志"),
            "查日志"
        );
        assert_eq!(display_hint("mcp_skywalking_skywalking_query"), "查日志");
        assert_eq!(display_hint("skywalking_query"), "查日志");
        assert_eq!(display_hint("mcp__filesystem__read_file"), "读文件");
        assert_eq!(display_hint("mcp__unknown__do_thing"), "外部工具");
        // 内联文案不受影响。
        assert_eq!(display_hint("grep src/lib.rs"), "搜索内容 src/lib.rs");

        let mut state = State::default();
        state.begin("s", "wecom-preflight-mcp", at(0));
        state.hint("wecom-mcp", "mcp__skywalking__skywalking_query", at(1));
        let pulse = state.bubble_pulse("wecom-mcp", at(30)).expect("pulse");
        assert!(pulse.contains("查日志"), "{pulse}");
        assert!(!pulse.contains("mcp__"), "气泡不应暴露协议名：{pulse}");
        assert!(!pulse.contains("skywalking__"), "{pulse}");
    }

    #[test]
    fn sanitize_hint_truncates_and_flattens() {
        let long = "字".repeat(MAX_HINT_CHARS + 5);
        let out = sanitize_hint(&format!("  a\tb\n{long}  "));
        assert!(!out.contains('\n'));
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= MAX_HINT_CHARS + 1);
        assert!(sanitize_hint("   \n\t  ").is_empty());
    }

    #[test]
    fn visible_answer_suppresses_heartbeats_but_not_status_inquiries() {
        let mut state = State::default();
        state.begin("s", "wecom-preflight-1", at(0));
        state.phase("wecom-1", "planning", at(1));
        assert!(state.heartbeat("wecom-1", at(30)).is_some());
        state.phase("wecom-1", "final_model", at(31));
        assert!(state.heartbeat("wecom-1", at(60)).is_some());
        state.visible_output("wecom-preflight-1", at(61));
        for seconds in [90, 120, 300] {
            state.activity("wecom-1", at(seconds));
            assert!(state.heartbeat("wecom-1", at(seconds)).is_none());
            assert!(state
                .status_reply("s", at(seconds))
                .contains("生成最终答复"));
        }
        state.finish("wecom-1", "completed", "completed", at(301));
        state.visible_output("wecom-1", at(302));
        assert_eq!(state.tasks[0].last_visible, at(61));
        state.begin("other", "another", at(303));
        assert!(state.heartbeat("another", at(333)).is_some());
        state.visible_output("absent", at(334));
        assert_eq!(state.tasks.len(), 2);
    }

    #[test]
    fn first_final_freezes_reason_status_phase_count_and_elapsed() {
        let mut state = State::default();
        state.begin("s", "r", at(0));
        state.phase("r", "planning", at(1));
        state.collected("r", 3, at(2));
        state.finish("r", "cancelled", "user_cancelled", at(9));
        state.finish("r", "completed", "completed", at(90));
        state.phase("r", "final_model", at(91));
        state.activity("r", at(92));
        state.collected("r", 999, at(93));
        assert!(!state.begin("s", "r", at(94)));
        let reply = state.status_reply("s", at(100));
        assert!(reply.contains("已取消") && reply.contains("用户取消"));
        assert!(reply.contains("制定查询计划") && reply.contains("耗时：9秒"));
        assert!(reply.contains("已采集：3条") && reply.contains("结束于91秒前"));
        assert_eq!(state.tasks[0].last_activity, at(2));
    }

    #[test]
    fn old_finish_does_not_overwrite_new_active_or_new_final() {
        let mut state = State::default();
        state.begin("s", "old", at(0));
        state.begin("s", "new", at(2));
        state.phase("new", "supplemental", at(3));
        state.finish("old", "cancelled", "user_cancelled", at(4));
        assert!(state.status_reply("s", at(5)).contains("补充采集"));
        state.finish("new", "completed", "completed", at(6));
        state.finish("old", "failed", "failed", at(7));
        assert!(state.status_reply("s", at(8)).contains("正常完成"));
        // Even when an old request's FIRST final arrives later, history is newest-begin.
        state.begin("x", "older", at(10));
        state.begin("x", "newer", at(11));
        state.finish("newer", "completed", "completed", at(12));
        state.finish("older", "failed", "failed", at(13));
        assert!(state.status_reply("x", at(14)).contains("正常完成"));
    }

    #[test]
    fn running_work_precedes_newer_queued_work_and_counts_waiting() {
        let mut state = State::default();
        state.begin("s", "running", at(0));
        state.phase("running", "acquisition", at(1));
        state.begin("s", "queued-one", at(2));
        state.begin("s", "queued-two", at(3));
        state.begin("other", "unrelated-queue", at(4));
        let reply = state.status_reply("s", at(5));
        assert!(reply.contains("采集资料"));
        assert!(reply.contains("另有2条任务排队等待"));
        state.finish("running", "completed", "completed", at(6));
        let reply = state.status_reply("s", at(7));
        assert!(reply.contains("排队中"));
        assert!(reply.contains("另有1条任务排队等待"));
        state.phase("queued-one", "planning", at(8));
        let reply = state.status_reply("s", at(9));
        assert!(reply.contains("制定查询计划"));
        assert!(reply.contains("另有1条任务排队等待"));
    }

    #[test]
    fn active_capacity_is_hard_bound_and_active_records_never_expire() {
        let mut state = State::default();
        for index in 0..CAPACITY {
            assert!(state.begin(&format!("s{index}"), &format!("r{index}"), at(0)));
        }
        assert!(!state.begin("overflow", "overflow", TTL + at(1)));
        assert_eq!(state.tasks.len(), CAPACITY);
        assert!(state.status_reply("s0", TTL + at(1)).contains("排队中"));
        state.finish("r7", "completed", "completed", TTL + at(2));
        assert!(state.begin("replacement", "replacement", TTL + at(3)));
        assert_eq!(state.tasks.len(), CAPACITY);
        assert!(state.tasks.iter().any(|task| task.request == "r0"));
        assert!(!state.tasks.iter().any(|task| task.request == "r7"));
    }

    #[test]
    fn capacity_reclaims_by_first_completion_not_start_or_inquiry_order() {
        let mut state = State::default();
        for index in 0..CAPACITY {
            state.begin("s", &format!("r{index}"), at(0));
        }
        state.finish("r100", "completed", "completed", at(1));
        state.finish("r0", "completed", "completed", at(2));
        state.finish("r100", "failed", "failed", at(3));
        state.status_reply("s", at(4));
        assert!(state.begin("new", "new", at(5)));
        assert!(!state.tasks.iter().any(|task| task.request == "r100"));
        assert!(state.tasks.iter().any(|task| task.request == "r0"));
    }

    #[test]
    fn inquiry_does_not_renew_ttl_create_work_or_advance_timestamps() {
        let mut state = State::default();
        assert_eq!(state.status_reply("empty", at(100)), NO_TASK);
        assert!(state.tasks.is_empty());
        state.begin("s", "r", at(0));
        state.finish("r", "completed", "completed", at(10));
        assert_ne!(state.status_reply("s", TTL + at(9)), NO_TASK);
        assert_eq!(state.status_reply("s", TTL + at(10)), NO_TASK);
        assert_eq!(state.observed, at(10));
        state.begin("other", "other", TTL + at(10));
        assert!(!state.tasks.iter().any(|task| task.request == "r"));
    }

    #[test]
    fn only_static_labels_and_counts_leave_the_state() {
        let mut state = State::default();
        let secret = "Bearer sk-super-secret <think>raw thought</think>";
        state.begin(secret, secret, at(0));
        state.phase(secret, secret, at(1));
        let status = state.status_reply(secret, at(10));
        let heartbeat = state.heartbeat(secret, at(30)).unwrap();
        state.finish(secret, secret, secret, at(31));
        let final_reply = state.status_reply(secret, at(40));
        for reply in [status, heartbeat, final_reply.clone()] {
            for forbidden in ["Bearer", "sk-super-secret", "<think>", "raw thought"] {
                assert!(!reply.contains(forbidden));
            }
        }
        assert!(final_reply.contains("unknown"));
        for phase in [
            "queued",
            "acquisition",
            "planning",
            "planning_repair",
            "supplemental",
            "final_model",
            "engine_ready",
            "session_setup",
        ] {
            assert_ne!(phase_label(phase), "处理中");
        }
        for reason in [
            "user_cancelled",
            "channel_changed",
            "timeout",
            "failed",
            "completed",
            "no_evidence",
            "busy",
        ] {
            assert_ne!(reason_label(reason), reason_label(secret));
        }
    }

    #[test]
    fn injected_time_regression_never_moves_state_backwards() {
        let mut state = State::default();
        state.begin("s", "r", at(50));
        state.activity("r", at(40));
        state.phase("r", "planning", at(30));
        assert_eq!(state.tasks[0].last_activity, at(50));
        assert!(state.status_reply("s", at(20)).contains("已用时：0秒"));
        assert!(state.heartbeat("r", at(79)).is_none());
        assert!(state.heartbeat("r", at(80)).is_some());
        state.finish("r", "completed", "completed", at(10));
        assert_eq!(state.tasks[0].terminal.as_ref().unwrap().at, at(80));
        assert!(state.status_reply("s", at(0)).contains("耗时：30秒"));
    }

    #[test]
    fn unknown_requests_and_invalid_begin_are_noops() {
        let mut state = State::default();
        assert!(!state.begin("", "r", at(0)));
        assert!(!state.begin("s", "", at(0)));
        assert!(!state.begin("s", &"x".repeat(MAX_KEY_BYTES + 1), at(0)));
        state.phase("absent", "planning", at(1));
        state.activity("absent", at(2));
        state.collected("absent", 99, at(3));
        state.finish("absent", "completed", "completed", at(4));
        assert!(state.heartbeat("absent", at(100)).is_none());
        assert!(state.tasks.is_empty());
    }

    /// 讲解类问答只到 final_model、从不调 collected：气泡不出现「已采集」。
    #[test]
    fn summary_omits_collected_until_log_acquisition_runs() {
        let mut state = State::default();
        state.begin("s", "wecom-1", at(0));
        state.phase("wecom-1", "final_model", at(5));
        let active = state.status_reply("s", at(30));
        assert!(active.contains("生成最终答复"));
        assert!(
            !active.contains("已采集"),
            "non-investigation turn leaked collected: {active}"
        );
        state.finish("wecom-1", "completed", "completed", at(40));
        let done = state.status_reply("s", at(50));
        assert!(done.contains("正常完成"));
        assert!(
            !done.contains("已采集"),
            "finished non-investigation turn leaked collected: {done}"
        );
    }

    /// 排障路径即使证据为 0 也会调 collected(…, 0)：仍展示「已采集：0条」。
    #[test]
    fn summary_keeps_zero_collected_after_log_acquisition() {
        let mut state = State::default();
        state.begin("s", "wecom-2", at(0));
        state.phase("wecom-2", "acquisition", at(1));
        state.collected("wecom-2", 0, at(2));
        let active = state.status_reply("s", at(10));
        assert!(
            active.contains("已采集：0条"),
            "zero evidence after acquisition must stay visible: {active}"
        );
        state.finish("wecom-2", "completed", "no_evidence", at(20));
        let done = state.status_reply("s", at(30));
        assert!(
            done.contains("已采集：0条"),
            "finished zero-evidence acquisition must stay visible: {done}"
        );
    }
}
