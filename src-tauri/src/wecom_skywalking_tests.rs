use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixed_args() -> Value {
    json!({"start":"2026-10-28T08:00:00Z", "end":"2026-10-28T09:00:00Z", "timezone_offset_minutes":480})
}

fn log(ts: i64, service: &str) -> Value {
    json!({"timestamp":ts, "serviceName":service, "serviceInstanceName":"instance-a", "content":"test", "tags":[]})
}

fn span() -> Value {
    json!({"segmentId":"seg", "spanId":0, "parentSpanId":-1, "serviceCode":"svc", "endpointName":"/test", "startTime":10, "endTime":20, "isError":false, "refs":[], "tags":[], "logs":[]})
}

// Only loopback sockets; each response consumes exactly one accepted HTTP request.
async fn mock(responses: Vec<(String, Duration)>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (response, delay) in responses {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 1024];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        let mut request: Value =
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                        request["_request_line"] = json!(headers.lines().next().unwrap());
                        requests.push(request);
                        break;
                    }
                }
            }
            tokio::time::sleep(delay).await;
            let _ = socket.write_all(response.as_bytes()).await;
        }
        requests
    });
    (addr, task)
}

#[derive(Default)]
struct HttpStats {
    connections: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    started: Notify,
}

struct KeepAliveOap {
    addr: String,
    stats: Arc<HttpStats>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for KeepAliveOap {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl KeepAliveOap {
    async fn new(
        handler: impl Fn(&Value, usize) -> (String, Duration) + Send + Sync + 'static,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let stats = Arc::new(HttpStats::default());
        let server_stats = stats.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            let mut sockets = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        server_stats.connections.fetch_add(1, Ordering::SeqCst);
                        let stats = server_stats.clone();
                        let handler = handler.clone();
                        sockets.spawn(async move {
                            loop {
                                let mut bytes = Vec::new();
                                let mut buf = [0; 2048];
                                let request = loop {
                                    let Ok(n) = socket.read(&mut buf).await else { return; };
                                    if n == 0 { return; }
                                    bytes.extend_from_slice(&buf[..n]);
                                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                                        let headers = String::from_utf8_lossy(&bytes[..end]);
                                        let length: usize = headers.lines().find_map(|line| {
                                            let (key, value) = line.split_once(':')?;
                                            key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                                        }).unwrap();
                                        if bytes.len() >= end + 4 + length {
                                            let mut req: Value = serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                                            req["_headers"] = json!(headers.to_string());
                                            break req;
                                        }
                                    }
                                };
                                let index = {
                                    let mut requests = stats.requests.lock().unwrap();
                                    requests.push(request.clone());
                                    requests.len() - 1
                                };
                                let active = stats.active.fetch_add(1, Ordering::SeqCst) + 1;
                                stats.peak.fetch_max(active, Ordering::SeqCst);
                                stats.started.notify_one();
                                let (wire, delay) = handler(&request, index);
                                tokio::time::sleep(delay).await;
                                let sent = socket.write_all(wire.replace("Connection: close", "Connection: keep-alive").as_bytes()).await;
                                stats.active.fetch_sub(1, Ordering::SeqCst);
                                if sent.is_err() { return; }
                            }
                        });
                    },
                    _ = sockets.join_next(), if !sockets.is_empty() => {},
                }
            }
        });
        Self { addr, stats, task }
    }
    fn captured(&self) -> Vec<Value> {
        self.stats.requests.lock().unwrap().clone()
    }
}

fn response(status: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len())
}

#[test]
fn graphql_endpoint_accepts_base_and_full_paths_without_duplication() {
    for (input, expected) in [
        ("http://host", "http://host/graphql"),
        ("http://host/base/", "http://host/base/graphql"),
        (" http://host/base/graphql/ ", "http://host/base/graphql"),
        ("http://host/base/graphql", "http://host/base/graphql"),
        (
            "https://host/base?token=secret#fragment",
            "https://host/base/graphql?token=secret",
        ),
    ] {
        assert_eq!(graphql_endpoint(input).unwrap(), expected);
    }
    for input in ["", "host/base", "ftp://host/base", "http://"] {
        assert!(graphql_endpoint(input)
            .unwrap_err()
            .starts_with("source_endpoint_error:"));
    }
}

#[test]
fn explicit_window_overrides_minutes_and_converts_oap_offset() {
    let args = json!({"start":"2026-10-25T08:01:02+00:00", "end":"2026-10-28T09:03:04Z", "minutes":1, "timezone_offset_minutes":480});
    let w = QueryWindow::from_args(&args, 240).unwrap();
    assert_eq!(
        w.duration(),
        json!({"start":"2026-10-25 160102", "end":"2026-10-28 170304", "step":"SECOND"})
    );
    let now = DateTime::parse_from_rfc3339("2026-10-28T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let relative = QueryWindow::at(
        &json!({"minutes":30,"timezone_offset_minutes":-300}),
        240,
        now,
    )
    .unwrap();
    assert_eq!(relative.duration()["start"], "2026-10-28 033000");
    assert_eq!(relative.end - relative.start, chrono::Duration::minutes(30));
}

#[test]
fn invalid_windows_fail_before_requests() {
    for args in [
        json!({"start":"2026-10-28T08:00:00Z"}),
        json!({"start":"2026-10-28 08:00:00","end":"2026-10-28T09:00:00Z"}),
        json!({"start":"2026-10-28T10:00:00Z","end":"2026-10-28T09:00:00Z"}),
        json!({"timezone_offset_minutes":1440}),
        json!({"timezone_offset_minutes":"480"}),
    ] {
        assert!(QueryWindow::from_args(&args, 240)
            .unwrap_err()
            .starts_with("schema_error:"));
    }
}

#[test]
fn malformed_envelopes_logs_and_spans_are_errors_not_empty_success() {
    for bytes in [
        b"no json".as_slice(),
        b"[]",
        b"{}",
        b"{\"data\":null}",
        b"{\"errors\":{},\"data\":{}}",
        b"{\"errors\":[{}],\"data\":{}}",
    ] {
        assert!(parse_graphql_body(bytes)
            .unwrap_err()
            .starts_with("schema_error:"));
    }
    let window = QueryWindow::from_args(&fixed_args(), 240).unwrap();
    for body in [
        json!({}),
        json!({"data":{"queryLogs":{"logs":null}}}),
        json!({"data":{"queryLogs":{"logs":[null]}}}),
        json!({"data":{"queryLogs":{"logs":[{"timestamp":0,"serviceName":"svc","content":false}]}}}),
    ] {
        assert!(parse_logs_page(&body, &window, "")
            .unwrap_err()
            .starts_with("schema_error:"));
    }
    for body in [
        json!({}),
        json!({"data":{"trace":{"spans":null}}}),
        json!({"data":{"trace":{"spans":[{}]}}}),
    ] {
        assert!(parse_trace_spans(&body)
            .unwrap_err()
            .starts_with("schema_error:"));
    }
    let mut invalid = span();
    invalid["refs"] = json!([{"parentSpanId":1}]);
    assert!(parse_trace_spans(&json!({"data":{"trace":{"spans":[invalid]}}})).is_err());
    let mut invalid = span();
    invalid["logs"] = json!([{"time":1,"data":[null]}]);
    assert!(parse_trace_spans(&json!({"data":{"trace":{"spans":[invalid]}}})).is_err());
}

#[test]
fn local_filter_preserves_raw_count_and_instance() {
    let w = QueryWindow::from_args(&fixed_args(), 240).unwrap();
    let start = w.start.timestamp_millis();
    let end = w.end.timestamp_millis();
    let body = json!({"data":{"queryLogs":{"logs":[log(start,"svc"),log(end,"svc"),log(end+1,"svc"),log(start,"other")]}}});
    let page = parse_logs_page(&body, &w, "svc").unwrap();
    assert_eq!(page.fetched_count, 4);
    assert_eq!(page.raw_count, 4);
    assert_eq!(page.min_ts, Some(start));
    assert_eq!(page.max_ts, Some(end + 1));
    assert_eq!(
        page.time_matched, 3,
        "time matching precedes service filtering"
    );
    assert_eq!(page.unknown_time_count, 0);
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[0].instance, "instance-a");
    let page = parse_logs_page(&body, &w, "absent").unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(page.fetched_count, 4);
    let empty = format_logs(&[], 60, 1, &fixed_args());
    assert!(empty.contains("不代表后续页为空"));
    assert!(empty.contains("2026-10-28T08:00:00Z"));
}

#[test]
fn child_budget_reserves_six_and_uses_absolute_shared_ceiling() {
    let parent = QueryBudget::new();
    let child = parent.child_limit(18);
    for _ in 0..18 {
        child.acquire().unwrap();
    }
    assert_eq!(child.remaining(), 0);
    assert!(child.clone().acquire().is_err());
    assert_eq!(parent.used(), 18);
    assert_eq!(parent.remaining(), 6);
    for _ in 0..6 {
        parent.acquire().unwrap();
    }
    assert!(parent.acquire().is_err());
    assert_eq!(child.used(), 24);

    let parent = QueryBudget::new();
    parent.acquire().unwrap();
    parent.acquire().unwrap();
    let child = parent.child_limit(3);
    assert_eq!(
        child.remaining(),
        3,
        "allowance starts after earlier HTTP calls"
    );
    let nested = child.child_limit(usize::MAX);
    for _ in 0..3 {
        nested.acquire().unwrap();
    }
    assert!(nested.acquire().is_err());
    assert_eq!(child.remaining(), 0);
    assert_eq!(parent.remaining(), 19);
}

#[tokio::test]
async fn exhausted_child_reuses_parent_session_cache_without_charging() {
    let body = json!({"data":{"trace":{"spans":[span()]}}}).to_string();
    let (addr, task) = mock(vec![(response("200 OK", &body), Duration::ZERO)]).await;
    let parent = QueryBudget::new();
    let child = parent.child_limit(1);
    fetch_trace_spans_budgeted(&addr, "", "tid", &child)
        .await
        .unwrap();
    assert_eq!(child.remaining(), 0);
    fetch_trace_spans_budgeted(&addr, "", "tid", &child)
        .await
        .unwrap();
    fetch_trace_spans_budgeted(&addr, "", "tid", &parent)
        .await
        .unwrap();
    assert_eq!(parent.used(), 1);
    assert_eq!(parent.cache_hits(), 2);
    assert_eq!(task.await.unwrap().len(), 1);
}

#[test]
fn raw_time_diagnostics_keep_zero_and_empty_extrema() {
    let window = QueryWindow::from_args(&fixed_args(), 240).unwrap();
    let start = window.start.timestamp_millis();
    let page = parse_logs_page(
        &json!({"data":{"queryLogs":{"logs":[log(0,"svc"),log(start,"svc")]}}}),
        &window,
        "",
    )
    .unwrap();
    assert_eq!(page.min_ts, Some(0));
    assert_eq!(page.max_ts, Some(start));
    assert_eq!(page.unknown_time_count, 1);
    assert_eq!(page.time_matched, 1);
    let empty = parse_logs_page(&json!({"data":{"queryLogs":{"logs":[]}}}), &window, "").unwrap();
    assert_eq!((empty.min_ts, empty.max_ts), (None, None));
    assert_eq!(empty.raw_count, 0);
}

#[test]
fn budget_clones_share_atomic_twenty_four_request_limit() {
    let budget = QueryBudget::new();
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let b = budget.clone();
            std::thread::spawn(move || b.acquire().is_ok())
        })
        .collect();
    let count = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(count, 24);
    assert_eq!(budget.used(), 24);
    assert_eq!(budget.remaining(), 0);
}

#[test]
fn custom_budget_is_shared_and_never_exceeds_hard_cap() {
    for (limit, expected) in [(0, 0), (2, 2), (usize::MAX, 24)] {
        let b = QueryBudget::with_limit(limit);
        assert_eq!(b.remaining(), expected);
        let clone = b.clone();
        for _ in 0..expected {
            clone.acquire().unwrap();
        }
        assert!(b.acquire().is_err());
        assert_eq!(b.used(), expected);
        assert_eq!(b.remaining(), 0);
    }
    assert_eq!(QueryBudget::default().remaining(), 24);
}

#[tokio::test]
async fn session_reuses_connection_and_three_pages_resolve_service_once() {
    let oap = KeepAliveOap::new(|request, _| {
        let body = if request["query"] == QUERY_FIND_SERVICE {
            json!({"data":{"service":{"id":"sid"}}})
        } else {
            json!({"data":{"queryLogs":{"logs":[]}}})
        };
        (response("200 OK", &body.to_string()), Duration::ZERO)
    })
    .await;
    let b = QueryBudget::new();
    let mut args = fixed_args();
    args["service"] = json!("svc");
    for page in 1..=3 {
        args["page"] = json!(page);
        fetch_logs_page_budgeted(&oap.addr, "X-Tenant: A", &args, &b)
            .await
            .unwrap();
    }
    assert_eq!(b.used(), 4);
    assert_eq!(b.cache_hits(), 2);
    assert_eq!(
        oap.captured()
            .iter()
            .filter(|r| r["query"] == QUERY_FIND_SERVICE)
            .count(),
        1
    );
    assert_eq!(oap.stats.connections.load(Ordering::SeqCst), 1);
    // The same request in a new investigation must reach the server again.
    let next = QueryBudget::new();
    fetch_logs_page_budgeted(&oap.addr, "X-Tenant: A", &args, &next)
        .await
        .unwrap();
    assert_eq!(next.used(), 2);
    assert_eq!(next.cache_hits(), 0);
}

#[tokio::test]
async fn identical_concurrent_queries_share_one_http_request() {
    let body = json!({"data":{"trace":{"spans":[span()]}}}).to_string();
    let (addr, task) = mock(vec![(response("200 OK", &body), Duration::from_millis(30))]).await;
    let b = QueryBudget::with_limit(1);
    let clone = b.clone();
    let (a, other) = tokio::join!(
        fetch_trace_spans_budgeted(&addr, "", "tid", &b),
        fetch_trace_spans_budgeted(&addr, "", "tid", &clone)
    );
    assert_eq!(a.unwrap(), other.unwrap());
    assert_eq!(b.used(), 1);
    assert_eq!(b.cache_hits(), 1);
    assert_eq!(clone.cache_hits(), 1);
    assert_eq!(task.await.unwrap().len(), 1);
}

#[tokio::test]
async fn header_endpoint_and_variable_identity_do_not_cross_cache() {
    let oap = KeepAliveOap::new(|request, _| {
        let mut row = span();
        let headers = request["_headers"].as_str().unwrap();
        row["serviceCode"] = json!(if headers.contains("tenant-B") {
            "B"
        } else {
            "A"
        });
        (
            response(
                "200 OK",
                &json!({"data":{"trace":{"spans":[row]}}}).to_string(),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let b = QueryBudget::new();
    for tenant in ["A", "B", "A", "B"] {
        let result =
            fetch_trace_spans_budgeted(&oap.addr, &format!("X-Tenant: tenant-{tenant}"), "tid", &b)
                .await
                .unwrap();
        assert_eq!(result[0]["serviceCode"], tenant);
    }
    assert_eq!(b.used(), 2);
    assert_eq!(b.cache_hits(), 2);
    assert!(!format!("{b:?}").contains("serviceCode"));
    assert!(!format!("{b:?}").contains("tenant-A"));
    fetch_trace_spans_budgeted(&oap.addr, "X-Tenant: tenant-A", "different-tid", &b)
        .await
        .unwrap();
    fetch_trace_spans_budgeted(
        &format!("{}/different", oap.addr),
        "X-Tenant: tenant-A",
        "tid",
        &b,
    )
    .await
    .unwrap();
    assert_eq!(b.used(), 4);
    assert_eq!(oap.stats.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn endpoint_admission_is_shared_across_sessions_and_headers() {
    let oap = KeepAliveOap::new(|_, _| {
        (
            response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#),
            Duration::from_millis(20),
        )
    })
    .await;
    let b = QueryBudget::new();
    let mut work = tokio::task::JoinSet::new();
    for i in 0..32 {
        let addr = oap.addr.clone();
        let b = b.clone();
        work.spawn(async move {
            fetch_trace_spans_budgeted(&addr, &format!("X-Tenant: {i}"), &format!("t{i}"), &b).await
        });
    }
    let mut successes = 0;
    while let Some(result) = work.join_next().await {
        match result.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => assert!(error.contains("budget exhausted"), "{error}"),
        }
    }
    assert_eq!(successes, 24);
    assert_eq!(b.used(), 24);
    assert_eq!(oap.captured().len(), 24);
    assert_eq!(oap.stats.peak.load(Ordering::SeqCst), 2);
    // Independent investigations still share endpoint pressure, not response data.
    for i in 0..8 {
        let addr = oap.addr.clone();
        work.spawn(async move {
            fetch_trace_spans_budgeted(
                &addr,
                &format!("X-Tenant: {i}"),
                "same",
                &QueryBudget::new(),
            )
            .await
        });
    }
    while let Some(result) = work.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(oap.captured().len(), 32);
    assert_eq!(oap.stats.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn http_trace_returns_unmodified_spans_and_counts_request() {
    let raw = span();
    let body = json!({"data":{"trace":{"spans":[raw.clone()]}}}).to_string();
    let (addr, task) = mock(vec![(response("200 OK", &body), Duration::ZERO)]).await;
    let b = QueryBudget::new();
    let spans = fetch_trace_spans_budgeted(&addr, "", "tid", &b)
        .await
        .unwrap();
    assert_eq!(spans, vec![raw]);
    assert_eq!(b.used(), 1);
    let requests = task.await.unwrap();
    assert_eq!(requests[0]["variables"]["traceId"], "tid");
}

#[tokio::test]
async fn source_probe_checks_logs_capability_in_same_window_with_two_requests() {
    for logs in [json!([]), json!([log(1793174400000, "some-service")])] {
        let (addr, task) = mock(vec![
            (
                response("200 OK", r#"{"data":{"__typename":"Query"}}"#),
                Duration::ZERO,
            ),
            (
                response(
                    "200 OK",
                    &json!({"data":{"queryLogs":{"logs":logs}}}).to_string(),
                ),
                Duration::ZERO,
            ),
        ])
        .await;
        let mut args = fixed_args();
        args["service"] = json!("target-not-proven");
        args["trace_id"] = json!("target-trace");
        args["keyword"] = json!("secret-order");
        let b = QueryBudget::with_limit(2);
        let source = probe_source(
            &format!("{addr}/base/graphql?token=secret-query"),
            "X-Test: secret-header",
            &args,
            &b,
        )
        .await
        .unwrap();
        assert_eq!(source.status, "ok");
        assert_eq!(source.endpoint, format!("{addr}/base/graphql"));
        assert!(source.detail.contains("不证明目标存在"));
        assert!(source.detail.contains("HTTP 200"));
        assert!(source.detail.contains("application/json"));
        assert!(!source.detail.contains("secret"));
        assert_eq!(b.used(), 2);
        let requests = task.await.unwrap();
        assert!(requests[0]["query"]
            .as_str()
            .unwrap()
            .contains("__typename"));
        for req in &requests {
            assert_eq!(
                req["_request_line"],
                "POST /base/graphql?token=secret-query HTTP/1.1"
            );
        }
        let c = &requests[1]["variables"]["condition"];
        assert_eq!(c["paging"], json!({"pageNum":1,"pageSize":1}));
        assert_eq!(
            c["queryDuration"],
            QueryWindow::from_args(&fixed_args(), 240)
                .unwrap()
                .duration()
        );
        assert!(c.get("serviceId").is_none());
        assert!(c.get("relatedTrace").is_none());
        assert!(c.get("keywordsOfContent").is_none());
    }
}

#[tokio::test]
async fn http_trace_log_filter_keeps_paging_without_server_time_or_service() {
    let body = json!({"data":{"queryLogs":{"logs":[log(1,"other")]}}}).to_string();
    let (addr, task) = mock(vec![(response("200 OK", &body), Duration::ZERO)]).await;
    let mut args = fixed_args();
    args["trace_id"] = json!("tid");
    args["service"] = json!("svc");
    args["page"] = json!(3);
    args["page_size"] = json!(1);
    let page = fetch_logs_page_budgeted(&addr, "", &args, &QueryBudget::new())
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    assert_eq!(page.fetched_count, 1);
    assert!(page.query_order_des);
    assert!(!page.duration_submitted);
    assert!(page.submitted_start.is_empty() && page.submitted_end.is_empty());
    assert!(page.window_start_ts > 0, "local window still retained");
    let requests = task.await.unwrap();
    let c = &requests[0]["variables"]["condition"];
    assert!(c.get("queryDuration").is_none());
    assert!(c.get("serviceId").is_none());
    assert_eq!(c["paging"], json!({"pageNum":3,"pageSize":1}));
    assert_eq!(c["relatedTrace"], json!({"traceId":"tid"}));
}

#[tokio::test]
async fn http_service_resolution_consumes_shared_budget_and_fixed_window() {
    let (addr, task) = mock(vec![
        (
            response("200 OK", r#"{"data":{"service":{"id":"real-id"}}}"#),
            Duration::ZERO,
        ),
        (
            response("200 OK", r#"{"data":{"queryLogs":{"logs":[]}}}"#),
            Duration::ZERO,
        ),
    ])
    .await;
    let mut args = fixed_args();
    args["service"] = json!("svc");
    let b = QueryBudget::new();
    fetch_logs_rows_budgeted(&addr, "", &args, &b)
        .await
        .unwrap();
    assert_eq!(b.used(), 2);
    let requests = task.await.unwrap();
    let c = &requests[1]["variables"]["condition"];
    assert_eq!(c["serviceId"], "real-id");
    assert_eq!(c["queryDuration"]["start"], "2026-10-28 160000");
    for _ in 2..24 {
        b.acquire().unwrap();
    }
    assert!(fetch_trace_spans_budgeted(&addr, "", "tid", &b)
        .await
        .unwrap_err()
        .contains("budget exhausted"));
}

#[tokio::test]
async fn source_probe_errors_keep_category_http_metadata_and_no_secrets() {
    for (status, body, category) in [
        (
            "405 Method Not Allowed",
            "secret-body",
            "source_endpoint_error:",
        ),
        ("404 Not Found", "secret-body", "source_endpoint_error:"),
        ("401 Unauthorized", "secret-body", "auth_error:"),
        (
            "200 OK",
            "secret-body",
            "schema_error: invalid GraphQL JSON",
        ),
        ("200 OK", r#"{"data":{}}"#, "schema_error: data.__typename"),
        (
            "200 OK",
            r#"{"errors":[{"message":"secret-body"}],"data":{}}"#,
            "schema_error:",
        ),
    ] {
        let wire =
            response(status, body).replace("application/json", "text/html; private=secret-header");
        let (addr, task) = mock(vec![(wire, Duration::ZERO)]).await;
        let addr = addr.replacen("http://", "http://secret-user:secret-password@", 1);
        let b = QueryBudget::new();
        let error = probe_source(
            &format!("{addr}/base/graphql?token=secret-query"),
            "Authorization: Bearer secret-auth",
            &fixed_args(),
            &b,
        )
        .await
        .unwrap_err();
        assert!(error.starts_with(category), "{error}");
        assert!(error.contains(&format!("HTTP {}", &status[..3])), "{error}");
        assert!(error.contains("content-type=text/html"), "{error}");
        assert!(error.contains("/base/graphql"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        assert_eq!(b.used(), 1);
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn source_probe_requires_query_logs_and_propagates_budget_exhaustion() {
    for second in [Some(r#"{"data":{"queryLogs":{"logs":null}}}"#), None] {
        let mut replies = vec![(
            response("200 OK", r#"{"data":{"__typename":"Query"}}"#),
            Duration::ZERO,
        )];
        if let Some(body) = second {
            replies.push((response("200 OK", body), Duration::ZERO));
        }
        let (addr, task) = mock(replies).await;
        let b = QueryBudget::with_limit(if second.is_some() { 2 } else { 1 });
        let error = probe_source(&addr, "", &fixed_args(), &b)
            .await
            .unwrap_err();
        if second.is_some() {
            assert!(
                error.starts_with("schema_error: data.queryLogs.logs"),
                "{error}"
            );
            assert!(error.contains("HTTP 200"), "{error}");
        } else {
            assert!(error.contains("budget exhausted"), "{error}");
        }
        assert_eq!(task.await.unwrap().len(), b.used());
    }
}

#[tokio::test]
async fn http_status_graphql_and_bad_structure_have_stable_categories() {
    let cases = [
        ("401 Unauthorized", "not json", "auth_error:"),
        ("403 Forbidden", "{}", "auth_error:"),
        ("500 Internal Server Error", "{}", "transport_error:"),
        ("302 Found", "{}", "transport_error:"),
        (
            "200 OK",
            r#"{"errors":[{"extensions":{"code":"UNAUTHENTICATED"}}],"data":{}}"#,
            "auth_error:",
        ),
        (
            "200 OK",
            r#"{"errors":[{"message":"bad"}],"data":{}}"#,
            "schema_error:",
        ),
        ("200 OK", r#"{"data":{"trace":{}}}"#, "schema_error:"),
    ];
    for (status, body, prefix) in cases {
        let (addr, task) = mock(vec![(response(status, body), Duration::ZERO)]).await;
        assert!(fetch_trace_spans(&addr, "", "tid")
            .await
            .unwrap_err()
            .starts_with(prefix));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn http_response_limit_checks_length_and_streamed_bytes() {
    let oversized_header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        MAX_RESPONSE_BYTES + 1
    );
    let payload = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",payload.len(),payload);
    for wire in [oversized_header, chunked] {
        let (addr, task) = mock(vec![(wire, Duration::ZERO)]).await;
        let err = fetch_trace_spans(&addr, "", "tid").await.unwrap_err();
        assert!(
            err.starts_with("schema_error:") && err.contains("4 MiB"),
            "{err}"
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn http_metrics_share_budget_and_explicit_window() {
    let bodies = [
        r#"{"data":{"metrics":[{"name":"endpoint_resp_time"},{"name":"endpoint_cpm"}]}}"#,
        r#"{"data":{"records":[]}}"#,
        r#"{"data":{"records":[]}}"#,
        r#"{"data":{"service":{"id":"sid"}}}"#,
        r#"{"data":{"instances":[]}}"#,
    ];
    let (addr, task) = mock(
        bodies
            .iter()
            .map(|body| (response("200 OK", body), Duration::ZERO))
            .collect(),
    )
    .await;
    let mut args = fixed_args();
    args["service"] = json!("svc");
    let b = QueryBudget::new();
    fetch_metrics_budgeted(&addr, "", &args, &b).await.unwrap();
    assert_eq!(b.used(), 5);
    let requests = task.await.unwrap();
    for i in [1, 2, 4] {
        assert_eq!(
            requests[i]["variables"]["duration"]["start"],
            "2026-10-28 160000"
        );
    }
}

#[test]
fn trace_display_keeps_tail_error_and_ancestors_and_labels_ambiguous_refs() {
    let mut spans: Vec<Value> = (0..150)
        .map(|i| {
            let mut s = span();
            s["spanId"] = json!(i);
            s["endpointName"] = json!(format!("/normal-{i}"));
            s
        })
        .collect();
    spans[149]["isError"] = json!(true);
    spans[149]["endpointName"] = json!("/tail-error");
    spans[149]["parentSpanId"] = json!(148);
    let display = format_trace_spans(&spans);
    assert!(display.contains("/tail-error"));
    assert!(display.contains("/normal-148"));
    assert_eq!(select_display_spans(&spans).len(), MAX_SPANS);
    let mut ambiguous = span();
    ambiguous["refs"] = json!([{"parentSegmentId":"A","parentSpanId":1,"type":"CROSS_PROCESS"},{"parentSegmentId":"B","parentSpanId":2,"type":"CROSS_THREAD"}]);
    let display = format_trace_spans(&[ambiguous]);
    assert!(display.contains("唯一父关系未确定"));
    assert!(display.contains("ref=A"));
    assert!(display.contains("ref=B"));
}

#[tokio::test]
async fn errors_partial_payloads_and_missing_services_are_never_cached() {
    for (status, bad_body) in [
        ("401 Unauthorized", "{}"),
        ("500 Internal Server Error", "{}"),
        ("200 OK", "broken-json"),
        (
            "200 OK",
            r#"{"errors":[{"message":"partial"}],"data":{"trace":{"spans":[]}}}"#,
        ),
        ("200 OK", r#"{"data":{"trace":{"spans":[{}]}}}"#),
        ("200 OK", r#"{"data":{"trace":{}}}"#),
    ] {
        let bad = response(status, bad_body);
        let oap = KeepAliveOap::new(move |_, index| {
            (
                if index == 0 {
                    bad.clone()
                } else {
                    response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#)
                },
                Duration::ZERO,
            )
        })
        .await;
        let b = QueryBudget::new();
        assert!(fetch_trace_spans_budgeted(&oap.addr, "", "tid", &b)
            .await
            .is_err());
        fetch_trace_spans_budgeted(&oap.addr, "", "tid", &b)
            .await
            .unwrap();
        fetch_trace_spans_budgeted(&oap.addr, "", "tid", &b)
            .await
            .unwrap();
        assert_eq!(b.used(), 2);
        assert_eq!(b.cache_hits(), 1);
        assert_eq!(oap.captured().len(), 2);
    }
    let oap = KeepAliveOap::new(|_, _| {
        (
            response("200 OK", r#"{"data":{"service":null}}"#),
            Duration::ZERO,
        )
    })
    .await;
    let b = QueryBudget::new();
    for _ in 0..2 {
        resolve_service_id_budgeted(&oap.addr, "", "absent", &b)
            .await
            .unwrap();
    }
    assert_eq!(b.used(), 2);
    assert_eq!(b.cache_hits(), 0);
}

#[tokio::test]
async fn cancelled_singleflight_leader_releases_waiters_for_retry() {
    let oap = KeepAliveOap::new(|_, index| {
        (
            response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#),
            if index == 0 {
                Duration::from_millis(150)
            } else {
                Duration::ZERO
            },
        )
    })
    .await;
    let b = QueryBudget::new();
    let addr = oap.addr.clone();
    let leader_budget = b.clone();
    let leader =
        tokio::spawn(
            async move { fetch_trace_spans_budgeted(&addr, "", "tid", &leader_budget).await },
        );
    tokio::time::timeout(Duration::from_secs(1), oap.stats.started.notified())
        .await
        .unwrap();
    let addr = oap.addr.clone();
    let follower_budget = b.clone();
    let follower = tokio::spawn(async move {
        fetch_trace_spans_budgeted(&addr, "", "tid", &follower_budget).await
    });
    tokio::task::yield_now().await;
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), follower)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(b.used(), 2);
    assert_eq!(b.cache_hits(), 0);
    fetch_trace_spans_budgeted(&oap.addr, "", "tid", &b)
        .await
        .unwrap();
    assert_eq!(b.cache_hits(), 1);
}

#[tokio::test]
async fn endpoint_wait_deadline_is_bounded_and_does_not_charge_budget() {
    let oap = KeepAliveOap::new(|_, _| {
        (
            response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#),
            Duration::from_millis(150),
        )
    })
    .await;
    let b = QueryBudget::new();
    let mut running = tokio::task::JoinSet::new();
    for id in ["one", "two"] {
        let addr = oap.addr.clone();
        let budget = b.clone();
        running.spawn(async move { fetch_trace_spans_budgeted(&addr, "", id, &budget).await });
        tokio::time::timeout(Duration::from_secs(1), oap.stats.started.notified())
            .await
            .unwrap();
    }
    let error = graphql_with_timeout(
        &oap.addr,
        "X-Tenant: other",
        QUERY_TRACE,
        json!({"traceId":"three"}),
        &b,
        Duration::from_millis(10),
    )
    .await
    .unwrap_err();
    assert!(error.starts_with("timeout:"));
    assert_eq!(b.used(), 2);
    assert_eq!(oap.captured().len(), 2);
    // A different OAP must progress while the first one's two permits are occupied.
    let other = KeepAliveOap::new(|_, _| {
        (
            response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#),
            Duration::ZERO,
        )
    })
    .await;
    graphql_with_timeout(
        &other.addr,
        "",
        QUERY_TRACE,
        json!({"traceId":"other"}),
        &b,
        Duration::from_millis(80),
    )
    .await
    .unwrap();
    while let Some(result) = running.join_next().await {
        result.unwrap().unwrap();
    }
    fetch_trace_spans_budgeted(&oap.addr, "X-Tenant: other", "three", &b)
        .await
        .unwrap();
    assert_eq!(b.used(), 4);
    assert_eq!(oap.stats.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn singleflight_wait_deadline_does_not_consume_http_budget() {
    let oap = KeepAliveOap::new(|_, _| {
        (
            response("200 OK", r#"{"data":{"trace":{"spans":[]}}}"#),
            Duration::from_millis(100),
        )
    })
    .await;
    let b = QueryBudget::new();
    let addr = oap.addr.clone();
    let leader_budget = b.clone();
    let leader =
        tokio::spawn(
            async move { fetch_trace_spans_budgeted(&addr, "", "tid", &leader_budget).await },
        );
    tokio::time::timeout(Duration::from_secs(1), oap.stats.started.notified())
        .await
        .unwrap();
    let err = graphql_with_timeout(
        &oap.addr,
        "",
        QUERY_TRACE,
        json!({"traceId":"tid"}),
        &b,
        Duration::from_millis(10),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("timeout:"));
    assert_eq!(b.used(), 1);
    leader.await.unwrap().unwrap();
    fetch_trace_spans_budgeted(&oap.addr, "", "tid", &b)
        .await
        .unwrap();
    assert_eq!(b.cache_hits(), 1);
}

#[tokio::test]
async fn http_timeout_is_classified_and_counts_attempt() {
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(20));
    let (addr, task) = mock(vec![(
        response("200 OK", r#"{"data":{}}"#),
        Duration::from_millis(100),
    )])
    .await;
    let b = QueryBudget::new();
    let err = graphql_with_timeout(
        &addr,
        "",
        QUERY_TRACE,
        json!({"traceId":"tid"}),
        &b,
        Duration::from_millis(25),
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("timeout:"));
    assert_eq!(b.used(), 1);
    task.await.unwrap();
}
