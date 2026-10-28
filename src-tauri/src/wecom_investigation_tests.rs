//! Offline HTTP integration fixtures for the investigation collector.
//! Included as a child module of wecom_investigation; no real OAP or model is used.
use super::*;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

struct MockOap {
    addr: String,
    requests: Arc<Mutex<Vec<Value>>>,
    accepted: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockOap {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl MockOap {
    async fn start<F>(response: F, delay: Duration) -> Self
    where
        F: Fn(&Value) -> (u16, Value) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(Notify::new());
        let seen = Arc::clone(&requests);
        let signal = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                let request = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    assert!(bytes.len() < 128 * 1024, "fixture request exceeded limit");
                    let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let header = String::from_utf8_lossy(&bytes[..header_end]);
                    let len: usize = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .expect("JSON request needs content length");
                    let body_start = header_end + 4;
                    if bytes.len() >= body_start + len {
                        break Some(
                            serde_json::from_slice::<Value>(&bytes[body_start..body_start + len])
                                .unwrap(),
                        );
                    }
                };
                let Some(request) = request else {
                    continue;
                };
                seen.lock().unwrap().push(request.clone());
                signal.notify_one();
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let (status, body) = response(&request);
                let body = body.to_string();
                let reason = if status == 200 { "OK" } else { "Unauthorized" };
                let header = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            addr,
            requests,
            accepted,
            task,
        }
    }
    fn captured(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn fixture_now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-10-28T12:00:00+08:00").unwrap()
}
fn fixture_query() -> InvestigationQuery {
    let mut q = parse_query("订单号 ORD123456 支付失败", None, fixture_now()).unwrap();
    q.keywords = vec!["ORD123456".into()];
    q.service = None;
    q.trace_id = None;
    q.page = 1;
    q
}
fn epoch() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-28T11:00:00+08:00")
        .unwrap()
        .timestamp_millis()
}
fn log(service: &str, trace: &str, content: &str, level: &str, index: i64) -> Value {
    json!({"serviceName":service,"serviceInstanceName":format!("{service}-1"),
        "endpointName":"/pay","timestamp":epoch()+index,"traceId":trace,"content":content,
        "tags":[{"key":"level","value":level}]})
}
fn log_page(rows: Vec<Value>) -> (u16, Value) {
    (200, json!({"data":{"queryLogs":{"logs":rows}}}))
}
fn span(service: &str, segment: &str, parent: Option<&str>, error: bool) -> Value {
    let refs = parent.map(|p| vec![json!({"traceId":"trace-main","parentSegmentId":p,"parentSpanId":0,"type":"CROSS_PROCESS"})]).unwrap_or_default();
    json!({"traceId":"trace-main","segmentId":segment,"spanId":0,"parentSpanId":-1,
        "refs":refs,"serviceCode":service,"serviceInstanceName":format!("{service}-1"),
        "endpointName":"/pay","type":"Entry","startTime":epoch(),"endTime":epoch()+1000,
        "isError":error,"layer":"Http","tags":[],"logs":[]})
}
fn chain() -> Vec<Value> {
    vec![
        span("gateway", "g", None, false),
        span("order", "o", Some("g"), false),
        span("payment", "p", Some("o"), true),
    ]
}
fn query_name(req: &Value, name: &str) -> bool {
    req["query"].as_str().unwrap_or("").contains(name)
}
fn service_response(req: &Value) -> (u16, Value) {
    let name = req["variables"]["serviceName"].as_str().unwrap();
    (
        200,
        json!({"data":{"service":{"id":format!("id-{name}"),"name":name}}}),
    )
}
async fn collect_fixture(oap: &MockOap, q: &InvestigationQuery, budget: &QueryBudget) -> Report {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut report = Report::default();
    report.next_page = q.page;
    tokio::time::timeout(
        Duration::from_secs(5),
        collect(&oap.addr, "", q, &[], &[], budget, &mut report, &tx),
    )
    .await
    .expect("collector hung on local HTTP fixture");
    report
}

#[tokio::test]
async fn entry_identifier_follows_gateway_order_payment_and_reads_trace_second_page() {
    let oap = MockOap::start(
        |req| {
            if query_name(req, "queryTrace") {
                return (200, json!({"data":{"trace":{"spans":chain()}}}));
            }
            assert!(query_name(req, "queryLogs"), "unexpected query: {req}");
            let c = &req["variables"]["condition"];
            if c["relatedTrace"]["traceId"] == "trace-main" {
                assert!(
                    c.get("keywordsOfContent").is_none(),
                    "entry identifier must not constrain downstream logs"
                );
                assert!(
                    c.get("serviceId").is_none(),
                    "entry service must not constrain downstream logs"
                );
                if c["paging"]["pageNum"] == 1 {
                    return log_page(
                        (0..40)
                            .map(|i| {
                                log(
                                    if i % 2 == 0 { "gateway" } else { "order" },
                                    "trace-main",
                                    &format!("normal downstream message {i}"),
                                    "INFO",
                                    i,
                                )
                            })
                            .collect(),
                    );
                }
                assert_eq!(c["paging"]["pageNum"], 2);
                return log_page(vec![log(
                    "payment",
                    "trace-main",
                    "PAYMENT_DATABASE_TIMEOUT",
                    "ERROR",
                    50,
                )]);
            }
            assert_eq!(c["keywordsOfContent"], json!(["ORD123456"]));
            log_page(vec![log(
                "gateway",
                "trace-main",
                "ORD123456 accepted",
                "INFO",
                0,
            )])
        },
        Duration::ZERO,
    )
    .await;
    let budget = QueryBudget::new();
    let q = fixture_query();
    let report = collect_fixture(&oap, &q, &budget).await;
    assert_eq!(report.traces.len(), 1);
    assert!(report
        .rows
        .iter()
        .any(|r| r.row.service == "order" && r.strength == "同trace精确关联"));
    assert!(report
        .rows
        .iter()
        .any(|r| r.row.content == "PAYMENT_DATABASE_TIMEOUT" && r.strength == "同trace精确关联"));
    assert!(render(&q, &report, budget.used()).contains("PAYMENT_DATABASE_TIMEOUT"));
    assert_eq!(
        oap.captured().len(),
        4,
        "entry, trace, trace log pages one and two"
    );
    assert_eq!(budget.used(), 4);
}

#[tokio::test]
async fn short_window_fallback_excludes_known_other_trace_and_labels_unlinked_candidate() {
    let oap = MockOap::start(
        |req| {
            if query_name(req, "queryTrace") {
                return (200, json!({"data":{"trace":{"spans":chain()}}}));
            }
            if query_name(req, "findService") {
                return service_response(req);
            }
            let c = &req["variables"]["condition"];
            if c["relatedTrace"]["traceId"] == "trace-main" {
                return log_page(vec![
                    log("gateway", "trace-main", "entry", "INFO", 0),
                    log("order", "trace-main", "order ok", "INFO", 1),
                ]);
            }
            if c.get("serviceId").is_some() {
                assert_eq!(c["serviceId"], "id-payment");
                assert!(c.get("keywordsOfContent").is_none());
                return log_page(vec![
                    log(
                        "payment",
                        "trace-other",
                        "OTHER_CUSTOMER_FAILURE",
                        "ERROR",
                        3,
                    ),
                    log("payment", "", "UNLINKED_PAYMENT_WARNING", "WARN", 4),
                ]);
            }
            log_page(vec![log("gateway", "trace-main", "ORD123456", "INFO", 0)])
        },
        Duration::ZERO,
    )
    .await;
    let budget = QueryBudget::new();
    let report = collect_fixture(&oap, &fixture_query(), &budget).await;
    assert!(!report
        .rows
        .iter()
        .any(|r| r.row.content == "OTHER_CUSTOMER_FAILURE"));
    let candidate = report
        .rows
        .iter()
        .find(|r| r.row.content == "UNLINKED_PAYMENT_WARNING")
        .expect("missing fallback evidence");
    assert_eq!(candidate.strength, "同服务短时间窗候选（未证实同一请求）");
    assert_eq!(
        budget.used(),
        oap.captured().len(),
        "service lookup must share the same HTTP budget"
    );
}

#[tokio::test]
async fn authentication_failure_is_not_reported_as_an_empty_success_or_saved_context() {
    let oap = MockOap::start(
        |_| (401, json!({"error":"secret-token-must-not-escape"})),
        Duration::ZERO,
    )
    .await;
    let id = format!("investigation-auth-{}", oap.addr);
    let _guard = crate::cancellation::CancelGuard::new(&id);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let output = investigate(
        &oap.addr,
        "Authorization: Bearer private-secret",
        &id,
        "订单号 ORD123456 支付失败",
        fixture_now(),
        &id,
        tx,
    )
    .await
    .unwrap();
    assert!(output.contains("auth_error"), "{output}");
    assert!(output.contains("部分完成"), "{output}");
    assert!(!output.contains("secret-token-must-not-escape"));
    assert!(!output.contains("private-secret"));
    assert!(contexts().lock().unwrap().get(&id).is_none());
    assert_eq!(oap.captured().len(), 1);
}

#[tokio::test]
async fn cancellation_during_http_wait_stops_collection_before_model_context_is_returned() {
    let oap = MockOap::start(|_| log_page(vec![]), Duration::from_secs(10)).await;
    let id = format!("investigation-cancel-{}", oap.addr);
    let _guard = crate::cancellation::CancelGuard::new(&id);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let work = investigate(
        &oap.addr,
        "",
        &id,
        "订单号 ORD123456 支付失败",
        fixture_now(),
        &id,
        tx,
    );
    let cancel = async {
        oap.accepted.notified().await;
        crate::cancellation::cancel_request(&id);
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(work, cancel) })
            .await
            .expect("cancel should interrupt an in-flight HTTP request");
    assert!(result.unwrap_err().contains("cancelled"));
    assert_eq!(oap.captured().len(), 1);
    assert!(contexts().lock().unwrap().get(&id).is_none());
}

#[tokio::test]
async fn explicit_trace_next_page_continues_after_last_collected_page() {
    let oap = MockOap::start(
        |req| {
            if query_name(req, "queryTrace") {
                return (200, json!({"data":{"trace":{"spans":[]}}}));
            }
            assert!(query_name(req, "queryLogs"));
            let c = &req["variables"]["condition"];
            assert_eq!(c["relatedTrace"]["traceId"], "trace-main");
            let page = c["paging"]["pageNum"].as_i64().unwrap();
            log_page(
                (0..40)
                    .map(|i| {
                        log(
                            "gateway",
                            "trace-main",
                            &format!("page-{page}-record-{i}"),
                            "INFO",
                            page * 40 + i,
                        )
                    })
                    .collect(),
            )
        },
        Duration::ZERO,
    )
    .await;
    let id = format!("investigation-trace-pages-{}", oap.addr);
    let _guard = crate::cancellation::CancelGuard::new(&id);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let first = investigate(
        &oap.addr,
        "",
        &id,
        "查日志 traceId=trace-main",
        fixture_now(),
        &id,
        tx.clone(),
    )
    .await
    .unwrap();
    assert!(first.contains("page-2-record-"), "{first}");
    let second = investigate(&oap.addr, "", &id, "下一页", fixture_now(), &id, tx)
        .await
        .unwrap();
    assert!(
        second.contains("page-3-record-"),
        "trace follow-up repeated its first page: {second}"
    );
    let pages: Vec<i64> = oap
        .captured()
        .iter()
        .filter(|r| query_name(r, "queryLogs"))
        .map(|r| {
            r["variables"]["condition"]["paging"]["pageNum"]
                .as_i64()
                .unwrap()
        })
        .collect();
    assert_eq!(pages, vec![1, 2, 3, 4]);
    contexts().lock().unwrap().entries.remove(&id);
}

#[tokio::test]
async fn budget_exhaustion_retains_entry_evidence_without_starting_another_http_request() {
    let oap = MockOap::start(
        |req| {
            assert!(query_name(req, "queryLogs"));
            log_page(vec![log(
                "gateway",
                "trace-main",
                "ORD123456 accepted",
                "INFO",
                0,
            )])
        },
        Duration::ZERO,
    )
    .await;
    let budget = QueryBudget::new();
    let q = fixture_query();
    for _ in 0..15 {
        sky::fetch_logs_page_budgeted(&oap.addr, "", &query_args(&q), &budget)
            .await
            .unwrap();
    }
    let report = collect_fixture(&oap, &q, &budget).await;
    assert_eq!(budget.used(), 16);
    assert_eq!(oap.captured().len(), 16);
    assert!(!report.rows.is_empty());
    assert!(report.traces.is_empty());
    assert!(report.partial);
    assert!(report.notes.iter().any(|n| n.contains("budget_exceeded")));
}
