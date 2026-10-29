//! In-memory task progress, keyed by the caller's exact SessionKey::as_str().
//! No logs, model text, tool arguments, network or disk access. Labels are static.
//! The orchestrator owns lifecycle and a separate timer calling `heartbeat`.
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CAPACITY: usize = 256;
const TTL: Duration = Duration::from_secs(2 * 60 * 60);
const HEARTBEAT: Duration = Duration::from_secs(30);
const MAX_KEY_BYTES: usize = 4096;
const NO_TASK: &str = "当前会话暂无任务记录。";

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
    collected: usize,
    terminal: Option<Final>,
}

impl Task {
    fn expired(&self, now: Duration) -> bool {
        self.terminal
            .as_ref()
            .is_some_and(|end| now.saturating_sub(end.at) >= TTL)
    }

    fn summary(&self, now: Duration) -> String {
        let end = self.terminal.as_ref().map_or(now, |end| end.at);
        let elapsed = end.saturating_sub(self.started).as_secs();
        let idle = now.saturating_sub(self.last_activity).as_secs();
        match &self.terminal {
            Some(end) => format!(
                "最近任务：{}；结束原因：{}；最后阶段：{}；耗时：{}秒；结束于{}秒前；已采集：{}条。",
                end.status, end.reason, self.phase, elapsed,
                now.saturating_sub(end.at).as_secs(), self.collected
            ),
            None => format!(
                "当前任务：{}；阶段：{}；已用时：{}秒；最近活动距今：{}秒；已采集：{}条。",
                if self.phase == phase_label("queued") { "排队中" } else { "进行中" },
                self.phase, elapsed, idle, self.collected
            ),
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
            collected: 0,
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

    fn collected(&mut self, request: &str, count: usize, now: Duration) {
        let now = self.time(now);
        if let Some(task) = self.active_mut(request) {
            // The caller supplies a cumulative snapshot, not an increment.
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
            summary.push_str(&format!(" 同会话另有{}条任务排队等待。", waiting));
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
/// No IO is performed.
pub fn heartbeat(request: &str) -> Option<String> {
    with_state(|state, now| state.heartbeat(request, now))
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
}
