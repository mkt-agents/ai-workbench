//! Request-scoped timing only: no prompts, code, headers or thought text is retained.
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_REQUESTS: usize = 256;

#[derive(Default)]
struct PhaseMetrics {
    elapsed_ms: u64,
    calls: u64,
    input_chars: u64,
    output_chars: u64,
    tools: u64,
    first_ms: Option<u64>,
    first_event_calls: u64,
    first_event_total_ms: u64,
}
impl PhaseMetrics {
    fn record(&mut self, phase: &Phase, elapsed_ms: u64) {
        self.elapsed_ms = self.elapsed_ms.saturating_add(elapsed_ms);
        self.calls = self.calls.saturating_add(1);
        self.input_chars = self.input_chars.saturating_add(phase.input_chars as u64);
        self.output_chars = self.output_chars.saturating_add(phase.output_chars as u64);
        self.tools = self.tools.saturating_add(phase.tools as u64);
        if let Some(ms) = phase.first_ms {
            self.first_ms = Some(self.first_ms.map_or(ms, |old| old.min(ms)));
            self.first_event_calls = self.first_event_calls.saturating_add(1);
            self.first_event_total_ms = self.first_event_total_ms.saturating_add(ms);
        }
    }
    fn value(&self, name: &str) -> Value {
        json!({"phase":name,"elapsedMs":self.elapsed_ms,"calls":self.calls,
            "inputChars":self.input_chars,"outputChars":self.output_chars,"toolEvents":self.tools,
            "firstEventMs":self.first_ms,"firstEventCalls":self.first_event_calls,
            "firstEventTotalMs":self.first_event_total_ms})
    }
}

#[derive(Default)]
struct RequestMetrics {
    generation: u64,
    phases: BTreeMap<&'static str, PhaseMetrics>,
    session_action: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    queue_ms: Option<u64>,
    validation_failures: Vec<String>,
    validation_failure_count: u64,
}
impl RequestMetrics {
    fn value(&self, request: &str) -> Value {
        let mut value = json!({"requestId":request,
            "phases":self.phases.iter().map(|(name, phase)| phase.value(name)).collect::<Vec<_>>(),
            "sessionAction":self.session_action,"model":self.model,"effort":self.effort,
            "validationFailures":self.validation_failures,"validationFailureCount":self.validation_failure_count});
        if let Some(queue_ms) = self.queue_ms {
            value["queueMs"] = json!(queue_ms);
        }
        // Stage durations may overlap/nest: never add them to the outer total.
        if let Some(total) = self.phases.get("total") {
            value["totalMs"] = json!(total.elapsed_ms);
            value["elapsedWithQueueMs"] =
                json!(total.elapsed_ms.saturating_add(self.queue_ms.unwrap_or(0)));
        }
        value
    }
}

#[derive(Default)]
struct Cache {
    requests: HashMap<String, RequestMetrics>,
    order: VecDeque<String>,
    generation: u64,
    last: Option<(u64, Value)>,
}
impl Cache {
    fn ensure(&mut self, request: &str) -> &mut RequestMetrics {
        if !self.requests.contains_key(request) {
            if self.requests.len() >= MAX_REQUESTS {
                if let Some(oldest) = self.order.pop_front() {
                    self.requests.remove(&oldest);
                }
            }
            self.generation = self.generation.wrapping_add(1);
            self.requests.insert(
                request.into(),
                RequestMetrics {
                    generation: self.generation,
                    ..RequestMetrics::default()
                },
            );
            self.order.push_back(request.into());
        }
        self.requests.get_mut(request).expect("inserted request")
    }
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// Last completed phase, preserving the original UI fields.
pub fn latest() -> Option<Value> {
    let cache = cache().lock().ok()?;
    let (generation, mut value) = cache.last.clone()?;
    if let Some(request) = value["requestId"].as_str() {
        if let Some(metrics) = cache.requests.get(request) {
            if metrics.generation == generation {
                let summary = metrics.value(request);
                value["summary"] = summary;
            }
        }
    }
    Some(value)
}

/// Completed phase metrics for exactly this request; active phases are not estimates.
pub fn summary(request: &str) -> Option<Value> {
    cache()
        .lock()
        .ok()?
        .requests
        .get(request)
        .map(|v| v.value(request))
}

// These APIs accept identifiers only, never prompts, error messages, URLs or credentials.
// Reject malformed/oversized labels rather than retaining a prefix of arbitrary text.
fn label(value: &str, max: usize) -> String {
    if value.is_empty() || value.len() > max {
        return "unknown".into();
    }
    let lower = value.to_ascii_lowercase();
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-/:".contains(&b))
        || value.contains("://")
        || lower.starts_with("sk-")
        || lower.starts_with("bearer")
        || lower.starts_with("enc:")
    {
        "unknown".into()
    } else {
        value.into()
    }
}

fn model_label(model: &str) -> String {
    if let Ok(parts) = serde_json::from_str::<Vec<String>>(model) {
        if parts.len() == 2 {
            let provider = label(&parts[0], 64);
            let model = label(&parts[1], 96);
            return format!("{provider}/{model}");
        }
    }
    label(model, 96)
}

/// Record only the selected model's public identifier and session action/effort labels.
/// Repeated calls update labels without clearing timings or concurrent phases.
pub fn record_session(request: &str, action: &str, model: &str, effort: &str) {
    if let Ok(mut cache) = cache().lock() {
        let metrics = cache.ensure(request);
        metrics.session_action = Some(label(action, 48));
        metrics.model = Some(model_label(model));
        metrics.effort = Some(label(effort, 24));
    }
}

/// Accumulate queue intervals independently of the outer total timer.
pub fn mark_queue(request: &str, elapsed_ms: u64) {
    if let Ok(mut cache) = cache().lock() {
        let metrics = cache.ensure(request);
        metrics.queue_ms = Some(metrics.queue_ms.unwrap_or(0).saturating_add(elapsed_ms));
    }
}

/// Pass a stable failure code, not the parser's error text or rejected model output.
/// Keep duplicate failures (retries count); bound the code sample, not the total count.
pub fn note_validation(request: &str, code: &str) {
    if let Ok(mut cache) = cache().lock() {
        let metrics = cache.ensure(request);
        metrics.validation_failure_count = metrics.validation_failure_count.saturating_add(1);
        if metrics.validation_failures.len() < 64 {
            metrics.validation_failures.push(label(code, 64));
        }
    }
    #[cfg(not(test))]
    crate::wecom_diagnostics::record(request, "validation", "failed", Some(code), None, json!({}));
}

pub struct Phase {
    request: String,
    generation: Option<u64>,
    name: &'static str,
    start: Instant,
    input_chars: usize,
    output_chars: usize,
    tools: usize,
    first_ms: Option<u64>,
}
impl Phase {
    pub fn new(request: &str, name: &'static str, input_chars: usize) -> Self {
        if name != "total" { crate::wecom_progress::phase(request, name); }
        let generation = cache()
            .lock()
            .ok()
            .map(|mut c| c.ensure(request).generation);
        Self {
            request: request.into(),
            generation,
            name,
            start: Instant::now(),
            input_chars,
            output_chars: 0,
            tools: 0,
            first_ms: None,
        }
    }
    pub fn event(&mut self) {
        crate::wecom_progress::activity(&self.request);
        if self.first_ms.is_none() {
            self.first_ms = Some(elapsed_ms(self.start));
        }
    }
    pub fn tool(&mut self) {
        self.event();
        self.tools = self.tools.saturating_add(1);
    }
    pub fn output(&mut self, text: &str) {
        self.event();
        self.output_chars = self.output_chars.saturating_add(text.chars().count());
    }
}
fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}
impl Drop for Phase {
    fn drop(&mut self) {
        let elapsed_ms = elapsed_ms(self.start);
        let value = json!({"requestId":self.request,"phase":self.name,"elapsedMs":elapsed_ms,
            "inputChars":self.input_chars,"outputChars":self.output_chars,"toolEvents":self.tools,"firstEventMs":self.first_ms});
        if let Ok(mut cache) = cache().lock() {
            // An evicted request's late Drop must neither resurrect it nor mix with
            // a new lifetime using the same ID. Concurrent phases share a generation.
            if let Some(metrics) = cache.requests.get_mut(&self.request) {
                if Some(metrics.generation) == self.generation {
                    metrics
                        .phases
                        .entry(self.name)
                        .or_default()
                        .record(self, elapsed_ms);
                    let generation = metrics.generation;
                    cache.last = Some((generation, value.clone()));
                }
            }
        }
        #[cfg(not(test))]
        {
            crate::wecom_log::write("performance", &value.to_string());
            // A dropped phase measures elapsed time only; it is not a success assertion.
            crate::wecom_diagnostics::record(&self.request, self.name, "phase_ended", None, Some(elapsed_ms),
                json!({"inputChars":self.input_chars,"outputChars":self.output_chars,
                    "toolEvents":self.tools,"firstEventMs":self.first_ms}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Global public APIs are the subject under test; serialize fixtures, while
    // individual tests explicitly spawn concurrent producers.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn phase_value(request: &str, name: &str) -> Value {
        summary(request).unwrap()["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["phase"] == name)
            .unwrap()
            .clone()
    }

    #[test]
    fn same_id_concurrent_calls_accumulate_and_total_excludes_stages() {
        let _lock = TEST_LOCK.lock().unwrap();
        mark_queue("concurrent", 17);
        let total = Phase::new("concurrent", "total", 0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let mut phase = Phase::new("concurrent", "planning_repair", 10);
                    phase.start -= std::time::Duration::from_millis(10);
                    phase.output("私密");
                    phase.tool();
                });
            }
        });
        assert!(summary("concurrent").unwrap().get("totalMs").is_none());
        drop(total);
        let phase = phase_value("concurrent", "planning_repair");
        assert_eq!(phase["calls"], 8);
        assert!(phase["elapsedMs"].as_u64().unwrap() >= 80);
        assert_eq!(phase["inputChars"], 80);
        assert_eq!(phase["outputChars"], 16);
        assert_eq!(phase["toolEvents"], 8);
        assert_eq!(phase["firstEventCalls"], 8);
        assert_eq!(
            summary("concurrent").unwrap()["totalMs"],
            phase_value("concurrent", "total")["elapsedMs"]
        );
        assert_eq!(phase_value("concurrent", "total")["calls"], 1);
        assert_eq!(
            summary("concurrent").unwrap()["elapsedWithQueueMs"]
                .as_u64()
                .unwrap(),
            summary("concurrent").unwrap()["totalMs"].as_u64().unwrap() + 17
        );
        assert!(!summary("concurrent").unwrap().to_string().contains("私密"));
    }

    #[test]
    fn first_event_is_stable_and_empty_phase_is_not_a_zero_event() {
        let _lock = TEST_LOCK.lock().unwrap();
        let mut phase = Phase::new("first-event", "answer", 2);
        phase.event();
        let first = phase.first_ms;
        phase.tool();
        phase.output("正文");
        assert_eq!(phase.first_ms, first);
        drop(phase);
        drop(Phase::new("first-event", "answer", 3));
        let out = phase_value("first-event", "answer");
        assert_eq!(out["calls"], 2);
        assert_eq!(out["firstEventCalls"], 1);
        assert_eq!(out["firstEventMs"], json!(first));
        assert_eq!(out["firstEventTotalMs"], json!(first));
        drop(Phase::new("first-event", "no_event", 0));
        assert!(phase_value("first-event", "no_event")["firstEventMs"].is_null());
    }

    #[test]
    fn eviction_bounds_cache_and_late_drop_cannot_mix_reused_id() {
        let _lock = TEST_LOCK.lock().unwrap();
        let old = Phase::new("evicted-id", "planning_initial", 999);
        for i in 0..MAX_REQUESTS {
            mark_queue(&format!("eviction-{i}"), i as u64);
        }
        assert!(summary("evicted-id").is_none());
        assert!(summary("eviction-0").is_some());
        drop(Phase::new("evicted-id", "planning_initial", 7));
        drop(old);
        assert_eq!(
            phase_value("evicted-id", "planning_initial")["inputChars"],
            7
        );
        assert_eq!(phase_value("evicted-id", "planning_initial")["calls"], 1);
        assert!(summary("eviction-0").is_none());
        assert_eq!(cache().lock().unwrap().requests.len(), MAX_REQUESTS);
    }

    #[test]
    fn latest_tracks_its_own_request_and_never_joins_a_recreated_generation() {
        let _lock = TEST_LOCK.lock().unwrap();
        drop(Phase::new("latest-a", "answer", 4));
        record_session("latest-a", "reuse", "unknown", "high");
        mark_queue("latest-b", 9);
        let last = latest().unwrap();
        assert_eq!(last["requestId"], "latest-a");
        assert_eq!(last["inputChars"], 4);
        assert_eq!(last["summary"]["requestId"], "latest-a");
        assert_eq!(last["summary"]["sessionAction"], "reuse");
        assert!(last["summary"].get("queueMs").is_none());
        for i in 0..MAX_REQUESTS {
            mark_queue(&format!("latest-evict-{i}"), 0);
        }
        mark_queue("latest-a", 42);
        assert!(latest().unwrap().get("summary").is_none());
    }

    #[test]
    fn opaque_model_selection_is_decoded_without_retaining_credentials() {
        assert_eq!(model_label(r#"["provider","model-v2"]"#), "provider/model-v2");
        assert_eq!(model_label(r#"["https://secret.example/key","sk-secret"]"#), "unknown/unknown");
        assert_eq!(model_label("Bearer private"), "unknown");
    }

    #[test]
    fn labels_are_bounded_and_failure_count_survives_sample_cap() {
        let _lock = TEST_LOCK.lock().unwrap();
        record_session("labels", "created", &"x".repeat(97), "Bearer secret");
        for _ in 0..70 {
            note_validation("labels", "PLAN_FORMAT_INVALID");
        }
        let out = summary("labels").unwrap();
        assert_eq!(out["model"], "unknown");
        assert_eq!(out["effort"], "unknown");
        assert_eq!(out["validationFailures"].as_array().unwrap().len(), 64);
        assert_eq!(out["validationFailureCount"], 70);
        record_session(
            "labels",
            "https://private.example",
            "sk-super-secret",
            "high",
        );
        note_validation("labels-invalid", "failure includes private body");
        assert_eq!(summary("labels").unwrap()["sessionAction"], "unknown");
        assert_eq!(summary("labels").unwrap()["model"], "unknown");
        assert_eq!(
            summary("labels-invalid").unwrap()["validationFailures"][0],
            "unknown"
        );
    }

    #[test]
    fn session_queue_and_validation_labels_are_request_scoped() {
        let _lock = TEST_LOCK.lock().unwrap();
        record_session("session-a", "created", "deepseek-chat", "high");
        mark_queue("session-a", 17);
        mark_queue("session-a", 3);
        note_validation("session-a", "PLAN_FORMAT_INVALID");
        note_validation("session-a", "PLAN_FORMAT_INVALID");
        let out = summary("session-a").unwrap();
        assert_eq!(out["sessionAction"], "created");
        assert_eq!(out["model"], "deepseek-chat");
        assert_eq!(out["effort"], "high");
        assert_eq!(out["queueMs"], 20);
        assert_eq!(
            out["validationFailures"],
            json!(["PLAN_FORMAT_INVALID", "PLAN_FORMAT_INVALID"])
        );
        assert_eq!(out["validationFailureCount"], 2);
        assert!(out.get("totalMs").is_none());
    }

    #[test]
    fn overlapping_requests_keep_independent_summaries() {
        let _lock = TEST_LOCK.lock().unwrap();
        let first = Phase::new("overlap-a", "planning_initial", 12);
        let second = Phase::new("overlap-b", "planning_initial", 24);
        drop(second);
        drop(first);
        assert_eq!(summary("overlap-a").unwrap()["phases"][0]["inputChars"], 12);
        assert_eq!(summary("overlap-b").unwrap()["phases"][0]["inputChars"], 24);
    }

    #[test]
    fn phase_counts_without_storing_text() {
        let _lock = TEST_LOCK.lock().unwrap();
        let mut p = Phase::new("perf-fixture", "planning", 12);
        p.tool();
        p.output("private正文");
        assert_eq!(p.output_chars, 9);
        assert_eq!(p.tools, 1);
        assert!(p.first_ms.is_some());
        drop(p);
        let out = latest().unwrap();
        assert_eq!(out["inputChars"], 12);
        assert!(!out.to_string().contains("private"));
    }
}
