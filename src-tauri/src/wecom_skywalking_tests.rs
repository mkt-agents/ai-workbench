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
