//! SkyWalking（OAP）只读查询：供企微问答在「排查线上问题」时取日志与调用链。
//!
//! 走标准 GraphQL（`POST {addr}/graphql`），查询串与预设脚本
//! `scripts/skywalking-log-query.user.js` 同协议并对齐其关键细节：
//! - 日志条件按 `serviceId`（经 `findService` 查真实 id，不是 serviceName）；
//! - `queryDuration` 用 OAP UTC 偏移（默认本机）+ SECOND；start/end RFC3339 优先于 minutes；
//! - `relatedTrace` 与 `queryDuration/serviceId` 互斥（有 traceId 时只给它）；
//! - `queryOrder: DES`（最新在前）。
//!
//! 地址由设置页下发到 `WecomState.skywalking`（空 = 不下发 `skywalking_query` 工具）。
//! 可选 header（鉴权版 OAP）由 `skywalking_header` 以 `Key: Value` 形式下发。

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, Utc};

/// 一个排障回合共享的实际 HTTP 请求预算。Clone 共享计数，失败请求也消耗预算。
#[derive(Debug, Clone, Default)]
pub struct QueryBudget {
    used: Arc<AtomicUsize>,
}

impl QueryBudget {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
    pub fn remaining(&self) -> usize {
        16usize.saturating_sub(self.used())
    }
    fn acquire(&self) -> Result<(), String> {
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 16).then_some(n + 1)
            })
            .map(|_| ())
            .map_err(|_| "transport_error: query budget exhausted (16 requests)".into())
    }
}

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

use serde_json::{json, Value};

const QUERY_LOGS: &str = "query queryLogs($condition: LogQueryCondition) { queryLogs(condition: $condition) { logs { serviceName serviceInstanceName endpointName traceId timestamp content tags { key value } } } }";
/// 带 `refs`：跨 segment 的父子关系要靠它重建调用树（与预设脚本 v7.1.2 逐字对齐）。
const QUERY_TRACE: &str = "query queryTrace($traceId: ID!) { trace: queryTrace(traceId: $traceId) { spans { traceId segmentId spanId parentSpanId refs { traceId parentSegmentId parentSpanId type } serviceCode serviceInstanceName endpointName type startTime endTime isError layer tags { key value } logs { time data { key value } } } } }";
/// 按服务名查真实 serviceId（和预设脚本同路径，比自己拼 base64 可靠）。
const QUERY_FIND_SERVICE: &str = "query findService($serviceName: String!) { service: findService(serviceName: $serviceName) { id name } }";
/// 端点 TopN（延迟 / 吞吐）：排障时定位「哪个端点在恶化」。
const QUERY_SORT_METRICS: &str = "query sortMetrics($condition: TopNCondition!, $duration: Duration!) { records: sortMetrics(condition: $condition, duration: $duration) { name id value } }";
/// 服务实例清单：配合指标看「哪个实例在冒烟」。
const QUERY_INSTANCES: &str = "query listInstances($serviceId: ID!, $duration: Duration!) { instances: listInstances(serviceId: $serviceId, duration: $duration) { id name } }";
/// 探测 OAP 实际提供了哪些指标名（不同版本/探针差异很大，先探再查避免整条报错）。
const QUERY_LIST_METRICS: &str =
    "query listMetrics($regex: String) { metrics: listMetrics(regex: $regex) { name } }";

/// 默认日志时间窗（分钟）：不指定时回看最近 4 小时。
/// 比 30 分钟宽，是因为用户常问「今天的日志」而消息里不带时间；
/// SkyWalking 的 Duration 粒度与窗口都在 OAP 侧按其配置时区解释。
pub const DEFAULT_WINDOW_MIN: i64 = 240;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 单次返回给模型的日志条数上限，避免把一次回合的上下文撑爆。
const MAX_LOGS: usize = 40;
const MAX_SPANS: usize = 120;
/// 分页上限：防止模型用超大 page_size 把上下文撑爆。
const MAX_PAGE_SIZE: i64 = 100;
/// 指标诊断里 TopN 端点的取样数。
const METRICS_TOPN: i64 = 10;
/// 日志摘要里最多列出的去重 traceId 数。
const MAX_TRACE_IDS: usize = 8;

/// 工具入口：按参数选 logs / trace / metrics。`addr` 来自渠道配置，`header` 可空。
pub async fn run(addr: &str, header: &str, args: &Value) -> Result<String, String> {
    let addr = addr.trim();
    if addr.is_empty() {
        return Err("transport_error: 未配置 SkyWalking 地址：请在「设置 → 联网扩展 → 企业微信机器人」填写 OAP 地址后再排查线上问题。".into());
    }
    // 显式要「指标 / 诊断」：查端点 TopN 延迟/吞吐 + 实例清单，给出根因趋势证据。
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind == "metrics" || kind == "diagnose" {
        return query_metrics(addr, header, args).await;
    }

    let trace_id = args
        .get("trace_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(tid) = trace_id {
        // 指定了 traceId 且没给其它日志条件 → 直接拉调用链；否则按日志查（relatedTrace）。
        let only_trace = args
            .get("service")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
            && args
                .get("keyword")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .is_empty();
        if only_trace && kind != "logs" {
            return query_trace(addr, header, tid).await;
        }
    }
    query_logs(addr, header, args).await
}

fn request_error(e: reqwest::Error) -> String {
    // 不把 URL/header 或远端响应正文带入错误，避免凭证与业务数据泄漏。
    if e.is_timeout() {
        "timeout: SkyWalking request exceeded 20s".into()
    } else {
        "transport_error: SkyWalking HTTP request failed".into()
    }
}

async fn graphql_budgeted(
    addr: &str,
    header: &str,
    query: &str,
    variables: Value,
    budget: &QueryBudget,
) -> Result<Value, String> {
    graphql_with_timeout(addr, header, query, variables, budget, REQUEST_TIMEOUT).await
}

async fn graphql_with_timeout(
    addr: &str,
    header: &str,
    query: &str,
    variables: Value,
    budget: &QueryBudget,
    timeout: Duration,
) -> Result<Value, String> {
    let addr = addr.trim();
    if addr.is_empty() {
        return Err("transport_error: 未配置 SkyWalking 地址".into());
    }
    let url = format!("{}/graphql", addr.trim_end_matches('/'));
    // 禁止自动重定向：每次实际请求都应计数，也避免把鉴权头转交其它地址。
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(request_error)?;
    let mut req = client
        .post(&url)
        .timeout(timeout)
        .json(&json!({ "query": query, "variables": variables }));
    for (k, v) in parse_headers(header) {
        req = req.header(k, v);
    }
    let req = req.build().map_err(request_error)?;
    budget.acquire()?;
    let mut resp = client.execute(req).await.map_err(request_error)?;
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(format!("auth_error: SkyWalking HTTP {}", status.as_u16()));
    }
    if !status.is_success() {
        return Err(format!(
            "transport_error: SkyWalking HTTP {}",
            status.as_u16()
        ));
    }
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err("schema_error: response exceeds 4 MiB".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(request_error)? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err("schema_error: response exceeds 4 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_graphql_body(&bytes)
}

fn parse_graphql_body(bytes: &[u8]) -> Result<Value, String> {
    let body: Value = serde_json::from_slice(bytes)
        .map_err(|_| "schema_error: invalid GraphQL JSON".to_string())?;
    if !body.is_object() {
        return Err("schema_error: GraphQL envelope must be an object".into());
    }
    if let Some(errors) = body.get("errors") {
        let errors = errors
            .as_array()
            .ok_or("schema_error: GraphQL errors must be an array")?;
        if !errors.is_empty() {
            let auth = errors.iter().any(|e| {
                matches!(
                    e.pointer("/extensions/code").and_then(Value::as_str),
                    Some("UNAUTHENTICATED" | "UNAUTHORIZED" | "FORBIDDEN")
                )
            });
            return Err(if auth {
                "auth_error: GraphQL authentication failed"
            } else {
                "schema_error: GraphQL returned errors"
            }
            .into());
        }
    }
    if !body.get("data").is_some_and(Value::is_object) {
        return Err("schema_error: GraphQL data must be an object".into());
    }
    Ok(body)
}

/// 一条结构化日志行（供文本渲染与链路闭环共用）。
#[derive(Debug, Clone)]
pub struct LogRow {
    /// epoch 毫秒
    pub ts: i64,
    pub service: String,
    pub instance: String,
    pub endpoint: String,
    pub level: String,
    pub thread: String,
    pub logger: String,
    pub trace_id: String,
    pub content: String,
}

/// 组日志查询条件（文本入口与结构化入口共用）。
fn build_log_condition(
    service_id: &str,
    endpoint_id: &str,
    keywords: &[String],
    exclude: &str,
    level: &str,
    trace_id: &str,
    duration: &Value,
    page: i64,
    page_size: i64,
) -> Value {
    let mut condition = serde_json::Map::new();
    condition.insert(
        "paging".into(),
        json!({ "pageNum": page, "pageSize": page_size }),
    );
    condition.insert("queryOrder".into(), json!("DES"));
    if !trace_id.is_empty() {
        // relatedTrace 与 queryDuration/serviceId 互斥。
        condition.insert("relatedTrace".into(), json!({ "traceId": trace_id }));
    } else {
        condition.insert("queryDuration".into(), duration.clone());
        if !service_id.is_empty() {
            condition.insert("serviceId".into(), json!(service_id));
            if !endpoint_id.is_empty() {
                condition.insert("endpointId".into(), json!(endpoint_id));
            }
        }
        if !keywords.is_empty() {
            // keywordsOfContent 是数组，多个词按 AND 收窄（如「订单号 + error」联合定位）。
            condition.insert("keywordsOfContent".into(), json!(keywords));
        }
        if !exclude.is_empty() {
            condition.insert("excludingKeywordsOfContent".into(), json!([exclude]));
        }
        if !level.is_empty() {
            // OAP 不支持该 tag 时整条会失败，故仅在显式指定时下发。
            condition.insert("tags".into(), json!([{ "key": "level", "value": level }]));
        }
    }
    Value::Object(condition)
}

/// 从 args 解析关键词列表：优先 `keywords` 数组，否则 `keyword` 字符串（支持逗号分隔多词）。
fn parse_keywords(args: &Value) -> Vec<String> {
    if let Some(arr) = args.get("keywords").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }
    let raw = str_arg(args, "keyword");
    raw.split([',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 把一条 queryLogs 返回的 JSON 日志解析成 LogRow（tags 里取 level/thread/logger，键名小写匹配）。
fn parse_log_row(log: &Value) -> Result<LogRow, String> {
    if !log.is_object() {
        return Err("schema_error: log must be an object".into());
    }
    let ts = integer_value(&log["timestamp"])
        .ok_or("schema_error: log.timestamp must be epoch milliseconds")?;
    for key in ["serviceName", "content"] {
        if !log.get(key).is_some_and(Value::is_string) {
            return Err(format!("schema_error: log.{key} must be a string"));
        }
    }
    for key in ["endpointName", "serviceInstanceName", "traceId"] {
        optional_string(log, key)?;
    }
    validate_tags(log.get("tags"))?;
    let mut level = String::new();
    let mut thread = String::new();
    let mut logger = String::new();
    if let Some(tags) = log.get("tags").and_then(|t| t.as_array()) {
        for t in tags {
            let k = t
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let v = t.get("value").and_then(|v| v.as_str()).unwrap_or("");
            match k.as_str() {
                "level" => level = v.to_string(),
                "thread" => thread = v.to_string(),
                "logger" => logger = v.to_string(),
                _ => {}
            }
        }
    }
    Ok(LogRow {
        ts,
        service: log
            .get("serviceName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        instance: log
            .get("serviceInstanceName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        endpoint: log
            .get("endpointName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        level,
        thread,
        logger,
        trace_id: log
            .get("traceId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        content: log
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
    })
}

fn integer_value(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

/// OAP Long 可能编码为 JSON 数字或十进制字符串；原始 span 保持原样。
pub fn span_i64(span: &Value, key: &str) -> Option<i64> {
    integer_value(&span[key])
}

fn optional_string(value: &Value, key: &str) -> Result<(), String> {
    if value
        .get(key)
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        return Err(format!("schema_error: {key} must be a string or null"));
    }
    Ok(())
}

fn validate_tags(value: Option<&Value>) -> Result<(), String> {
    if let Some(value) = value.filter(|v| !v.is_null()) {
        let tags = value
            .as_array()
            .ok_or("schema_error: tags must be an array")?;
        for tag in tags {
            if !tag.get("key").is_some_and(Value::is_string)
                || !tag.get("value").is_some_and(Value::is_string)
            {
                return Err("schema_error: tag key/value must be strings".into());
            }
        }
    }
    Ok(())
}

/// 原始页条数与筛选后行数分开，筛后空页不代表 OAP 分页结束。
#[derive(Debug, Clone)]
pub struct LogPage {
    pub rows: Vec<LogRow>,
    pub fetched_count: usize,
}

/// 结构化日志查询：供链路闭环（编排层预取）复用，返回 LogRow 列表。
pub async fn fetch_logs_rows(
    addr: &str,
    header: &str,
    args: &Value,
) -> Result<Vec<LogRow>, String> {
    fetch_logs_rows_budgeted(addr, header, args, &QueryBudget::new()).await
}

pub async fn fetch_logs_rows_budgeted(
    addr: &str,
    header: &str,
    args: &Value,
    budget: &QueryBudget,
) -> Result<Vec<LogRow>, String> {
    Ok(fetch_logs_page_budgeted(addr, header, args, budget)
        .await?
        .rows)
}

pub async fn fetch_logs_page_budgeted(
    addr: &str,
    header: &str,
    args: &Value,
    budget: &QueryBudget,
) -> Result<LogPage, String> {
    // 在服务 ID 解析之前固定窗口，后续请求不重新采样 now。
    let window = QueryWindow::from_args(args, DEFAULT_WINDOW_MIN)?;
    let service = str_arg(args, "service");
    let endpoint = str_arg(args, "endpoint");
    let keywords = parse_keywords(args);
    let exclude = str_arg(args, "exclude_keyword");
    let level = str_arg(args, "level");
    let trace_id = str_arg(args, "trace_id");
    let page = args
        .get("page")
        .and_then(|v| v.as_i64())
        .filter(|p| *p >= 1)
        .unwrap_or(1);
    let page_size = args
        .get("page_size")
        .and_then(|v| v.as_i64())
        .filter(|s| *s >= 1)
        .map(|s| s.min(MAX_PAGE_SIZE))
        .unwrap_or(MAX_LOGS as i64);

    // serviceName → 真实 id（trace 查询不需要）。
    let (sid, eid) = if trace_id.is_empty() && !service.is_empty() {
        let sid = resolve_service_id_budgeted(addr, header, &service, budget).await?;
        let eid = if endpoint.is_empty() {
            String::new()
        } else {
            endpoint_id(&sid, &endpoint)
        };
        (sid, eid)
    } else {
        (String::new(), String::new())
    };

    let condition = build_log_condition(
        &sid,
        &eid,
        &keywords,
        &exclude,
        &level,
        &trace_id,
        &window.duration(),
        page,
        page_size,
    );
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_LOGS,
        json!({ "condition": condition }),
        budget,
    )
    .await?;
    parse_logs_page(
        &body,
        &window,
        if trace_id.is_empty() { "" } else { &service },
    )
}

fn parse_logs_page(
    body: &Value,
    window: &QueryWindow,
    service_filter: &str,
) -> Result<LogPage, String> {
    let logs = body
        .pointer("/data/queryLogs/logs")
        .and_then(Value::as_array)
        .ok_or("schema_error: data.queryLogs.logs must be an array")?;
    // 先验证整页再筛选，坏行即使落在窗口之外也不能被静默吞掉。
    let rows: Vec<LogRow> = logs.iter().map(parse_log_row).collect::<Result<_, _>>()?;
    Ok(LogPage {
        fetched_count: logs.len(),
        rows: rows
            .into_iter()
            .filter(|row| {
                row.ts >= window.start.timestamp_millis()
                    && row.ts <= window.end.timestamp_millis()
                    && (service_filter.is_empty() || row.service == service_filter)
            })
            .collect(),
    })
}

/// 本页错误级条数 + 去重 traceId（最多 `MAX_TRACE_IDS` 个，错误行的 traceId 优先靠前）。
fn summarize_logs(rows: &[LogRow]) -> (usize, Vec<String>) {
    let mut err_count = 0usize;
    let mut ids: Vec<String> = Vec::new();
    // 两趟：先错误行的 traceId，再普通行，保证错误 trace 排在前。
    for only_err in [true, false] {
        for r in rows {
            let is_err = is_error_row(r);
            if only_err {
                if is_err {
                    err_count += 1;
                }
                if !is_err {
                    continue;
                }
            } else if is_err {
                continue;
            }
            let tid = r.trace_id.trim();
            if !tid.is_empty() && !ids.iter().any(|x| x == tid) && ids.len() < MAX_TRACE_IDS {
                ids.push(tid.to_string());
            }
        }
    }
    (err_count, ids)
}

/// 日志里第一个可追查的 traceId（错误行优先）；供链路闭环预取使用。
pub fn first_trace_id(rows: &[LogRow]) -> Option<String> {
    let (_err, ids) = summarize_logs(rows);
    ids.into_iter().next()
}

/// 预取闭环用的紧凑日志渲染：带错误摘要 + 最多 20 行（每行 level/时间/服务/trace + 截断内容）。
pub fn format_rows_brief(rows: &[LogRow]) -> String {
    const BRIEF_MAX: usize = 20;
    if rows.is_empty() {
        return "（无）".into();
    }
    let (err_count, trace_ids) = summarize_logs(rows);
    let mut out = String::new();
    if err_count > 0 || !trace_ids.is_empty() {
        out.push_str(&format!("摘要：错误级 {err_count} 条"));
        if !trace_ids.is_empty() {
            out.push_str(&format!("；traceId：{}", trace_ids.join(", ")));
        }
        out.push('\n');
    }
    for r in rows.iter().take(BRIEF_MAX) {
        let lvl = if r.level.is_empty() {
            String::new()
        } else {
            format!("[{}] ", r.level.to_ascii_uppercase())
        };
        let svc = if r.service.is_empty() {
            "-"
        } else {
            r.service.as_str()
        };
        let tid = if r.trace_id.is_empty() {
            String::new()
        } else {
            format!(" · trace={}", r.trace_id)
        };
        let content: String = r.content.chars().take(300).collect();
        out.push_str(&format!(
            "- {lvl}{} {svc}{tid}\n  {content}\n",
            fmt_ts(r.ts)
        ));
    }
    if rows.len() > BRIEF_MAX {
        out.push_str(&format!("…（另有 {} 条未展示）\n", rows.len() - BRIEF_MAX));
    }
    out
}

/// 一条日志是否「错误级」：level 命中 ERROR/FATAL/SEVERE，或内容含异常关键字。
fn is_error_row(r: &LogRow) -> bool {
    let lvl = r.level.to_ascii_uppercase();
    if matches!(lvl.as_str(), "ERROR" | "FATAL" | "SEVERE" | "ERR") {
        return true;
    }
    let c = r.content.to_ascii_lowercase();
    c.contains("exception")
        || c.contains("error")
        || c.contains("traceback")
        || c.contains("caused by")
        || r.content.contains("异常")
        || r.content.contains("报错")
}

/// epoch 毫秒 → 本地 `MM-DD HH:MM:SS`（0 或无效时给占位）。
fn fmt_ts(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    if ms <= 0 {
        return "--".into();
    }
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%m-%d %H:%M:%S").to_string(),
        None => "--".into(),
    }
}

/// 把 LogRow 列表渲染成带排障摘要的文本。
fn format_logs(rows: &[LogRow], minutes: i64, page: i64, args: &Value) -> String {
    let service = str_arg(args, "service");
    let endpoint = str_arg(args, "endpoint");
    let keywords = parse_keywords(args);
    let keyword_str = if keywords.is_empty() {
        String::new()
    } else {
        keywords.join("+")
    };
    let exclude = str_arg(args, "exclude_keyword");
    let level = str_arg(args, "level");
    let trace_id = str_arg(args, "trace_id");
    let window_label = if !str_arg(args, "start").is_empty() {
        format!("{} 至 {}", str_arg(args, "start"), str_arg(args, "end"))
    } else {
        format!("最近 {minutes} 分钟")
    };

    if rows.is_empty() {
        return format!(
            "SkyWalking 本页筛选后无日志（{window_label}，第 {page} 页，service={} endpoint={} keyword={} exclude={} level={} trace={}）。本页为空不代表后续页为空；可继续翻页或放宽条件。",
            none_if_empty(&service),
            none_if_empty(&endpoint),
            none_if_empty(&keyword_str),
            none_if_empty(&exclude),
            none_if_empty(&level),
            none_if_empty(&trace_id)
        );
    }

    let (err_count, trace_ids) = summarize_logs(rows);
    let mut out = format!(
        "SkyWalking 日志（{window_label}，第 {page} 页，筛选后 {} 条）：\n",
        rows.len()
    );
    // 排障摘要：错误条数 + 可追查的 traceId。
    if err_count > 0 || !trace_ids.is_empty() {
        out.push_str(&format!("摘要：错误级 {err_count} 条"));
        if !trace_ids.is_empty() {
            out.push_str(&format!(
                "；可追查 traceId（优先错误）：{}",
                trace_ids.join(", ")
            ));
        }
        out.push('\n');
    }
    for r in rows.iter() {
        let lvl = if r.level.is_empty() {
            String::new()
        } else {
            format!("[{}] ", r.level.to_ascii_uppercase())
        };
        let svc = if r.service.is_empty() {
            "-"
        } else {
            r.service.as_str()
        };
        let ep = if r.endpoint.is_empty() {
            "-"
        } else {
            r.endpoint.as_str()
        };
        let mut meta = String::new();
        if !r.instance.is_empty() {
            meta.push_str(&format!(" · instance={}", r.instance));
        }
        if !r.logger.is_empty() {
            meta.push_str(&format!(" · @{}", r.logger));
        }
        if !r.thread.is_empty() {
            meta.push_str(&format!(" · {}", r.thread));
        }
        if !r.trace_id.is_empty() {
            meta.push_str(&format!(" · trace={}", r.trace_id));
        }
        let content: String = r.content.chars().take(500).collect();
        out.push_str(&format!(
            "- {lvl}{} {svc} · {ep}{meta}\n  {content}\n",
            fmt_ts(r.ts)
        ));
    }
    out.push_str("\n排障提示：先看带 [ERROR] 的行及其 traceId，用该 trace_id 查调用链（kind 省略即返回调用链）定位根因；还要更多日志传 page 翻页；要端点/实例趋势传 kind=metrics。");
    out
}

async fn query_logs(addr: &str, header: &str, args: &Value) -> Result<String, String> {
    let minutes = args
        .get("minutes")
        .and_then(|v| v.as_i64())
        .filter(|m| *m > 0 && *m <= 24 * 60)
        .unwrap_or(DEFAULT_WINDOW_MIN);
    let page = args
        .get("page")
        .and_then(|v| v.as_i64())
        .filter(|p| *p >= 1)
        .unwrap_or(1);
    let result = fetch_logs_page_budgeted(addr, header, args, &QueryBudget::new()).await?;
    Ok(format!(
        "{}\nOAP 本页原始条数：{}；分页位置基于原始页，非筛选后条数。",
        format_logs(&result.rows, minutes, page, args),
        result.fetched_count
    ))
}

/// 返回完整、未归一化/截断的原始 spans，供编排层逐服务补查。
pub async fn fetch_trace_spans(
    addr: &str,
    header: &str,
    trace_id: &str,
) -> Result<Vec<Value>, String> {
    fetch_trace_spans_budgeted(addr, header, trace_id, &QueryBudget::new()).await
}

pub async fn fetch_trace_spans_budgeted(
    addr: &str,
    header: &str,
    trace_id: &str,
    budget: &QueryBudget,
) -> Result<Vec<Value>, String> {
    if trace_id.trim().is_empty() {
        return Err("schema_error: trace_id must not be empty".into());
    }
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_TRACE,
        json!({ "traceId": trace_id.trim() }),
        budget,
    )
    .await?;
    parse_trace_spans(&body)
}

fn parse_trace_spans(body: &Value) -> Result<Vec<Value>, String> {
    let spans = body
        .pointer("/data/trace/spans")
        .and_then(Value::as_array)
        .ok_or("schema_error: data.trace.spans must be an array")?;
    for span in spans {
        for key in ["segmentId", "serviceCode", "endpointName"] {
            if !span.get(key).is_some_and(Value::is_string) {
                return Err(format!("schema_error: span.{key} must be a string"));
            }
        }
        for key in ["spanId", "parentSpanId", "startTime", "endTime"] {
            if integer_value(&span[key]).is_none() {
                return Err(format!("schema_error: span.{key} must be an integer"));
            }
        }
        if !span.get("isError").is_some_and(Value::is_boolean) {
            return Err("schema_error: span.isError must be a boolean".into());
        }
        for key in ["traceId", "serviceInstanceName", "type", "layer"] {
            optional_string(span, key)?;
        }
        validate_tags(span.get("tags"))?;
        if let Some(refs) = span.get("refs").filter(|v| !v.is_null()) {
            for r in refs
                .as_array()
                .ok_or("schema_error: span.refs must be an array")?
            {
                if !r.get("parentSegmentId").is_some_and(Value::is_string)
                    || integer_value(&r["parentSpanId"]).is_none()
                {
                    return Err("schema_error: malformed span ref".into());
                }
                optional_string(r, "traceId")?;
                optional_string(r, "type")?;
            }
        }
        if let Some(logs) = span.get("logs").filter(|v| !v.is_null()) {
            for entry in logs
                .as_array()
                .ok_or("schema_error: span.logs must be an array")?
            {
                if integer_value(&entry["time"]).is_none()
                    || !entry.get("data").is_some_and(Value::is_array)
                {
                    return Err("schema_error: malformed span log".into());
                }
                validate_tags(entry.get("data"))?;
            }
        }
    }
    Ok(spans.clone())
}

async fn query_trace(addr: &str, header: &str, trace_id: &str) -> Result<String, String> {
    let spans = fetch_trace_spans(addr, header, trace_id).await?;
    if spans.is_empty() {
        return Ok(format!(
            "SkyWalking 未查到调用链 trace={trace_id}（可能已过期或 traceId 有误）。"
        ));
    }
    Ok(format!(
        "SkyWalking trace={trace_id}\n{}",
        format_trace_spans(&spans)
    ))
}

/// 文本摘要仅截断展示，不改变 fetch_trace_spans 的原始数组。
pub fn format_trace_spans(spans: &[Value]) -> String {
    let total = spans.len();
    let err_count = spans
        .iter()
        .filter(|s| s.get("isError").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();
    let mut out = format!(
        "SkyWalking 调用链（共 {total} 个 span，其中 {err_count} 个报错，最多展示 {MAX_SPANS}；按父子层级缩进）：\n"
    );
    let selected = select_display_spans(spans);
    for line in build_span_tree(&selected) {
        out.push_str(&line);
    }
    if selected.len() < spans.len() {
        out.push_str(&format!("\n展示截断：保留错误 span 优先、随后其祖先，省略 {} 个 span；缺失父节点可能位于省略部分。\n", spans.len() - selected.len()));
    }
    if err_count > 0 {
        out.push_str("\n定位到报错 span 后，可用其 serviceCode + 时间窗再查日志（传 service / keyword），或传 kind=metrics 看端点/实例趋势。");
    }
    out
}

fn select_display_spans(spans: &[Value]) -> Vec<Value> {
    use std::collections::{HashMap, VecDeque};
    if spans.len() <= MAX_SPANS {
        return spans.to_vec();
    }
    let mut index: HashMap<(String, i64), Vec<usize>> = HashMap::new();
    for (i, span) in spans.iter().enumerate() {
        index
            .entry((
                str_arg(span, "segmentId"),
                span_i64(span, "spanId").unwrap_or(-1),
            ))
            .or_default()
            .push(i);
    }
    let mut selected = vec![false; spans.len()];
    let mut queue = VecDeque::new();
    let mut count = 0;
    // 先容纳所有错误，避免一条长祖先链挤掉别的错误。
    for (i, span) in spans.iter().enumerate() {
        if span["isError"].as_bool() == Some(true) && count < MAX_SPANS {
            selected[i] = true;
            count += 1;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        let span = &spans[i];
        let mut parents = Vec::new();
        if let Some(refs) = span["refs"].as_array() {
            for r in refs {
                parents.push((
                    str_arg(r, "parentSegmentId"),
                    span_i64(r, "parentSpanId").unwrap_or(-1),
                ));
            }
        }
        if let Some(id) = span_i64(span, "parentSpanId").filter(|n| *n >= 0) {
            parents.push((str_arg(span, "segmentId"), id));
        }
        for key in parents {
            if let Some(ids) = index.get(&key) {
                for &p in ids {
                    if !selected[p] && count < MAX_SPANS {
                        selected[p] = true;
                        count += 1;
                        queue.push_back(p);
                    }
                }
            }
        }
    }
    for slot in &mut selected {
        if count == MAX_SPANS {
            break;
        }
        if !*slot {
            *slot = true;
            count += 1;
        }
    }
    spans
        .iter()
        .zip(selected)
        .filter_map(|(span, keep)| keep.then(|| span.clone()))
        .collect()
}

/// 用 segmentId+spanId 建索引，按 refs（跨段父子）/ parentSpanId（同段父子）重建调用树。
/// 返回每行已带缩进与出错明细的文本行（顺序 = 前序遍历，根按 startTime 排）。
fn build_span_tree(spans: &[Value]) -> Vec<String> {
    use std::collections::HashMap;
    // key: "segmentId:spanId" → index
    let key_of = |seg: &str, sp: i64| format!("{seg}:{sp}");
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    for (i, s) in spans.iter().enumerate() {
        let seg = s.get("segmentId").and_then(|v| v.as_str()).unwrap_or("");
        let sp = integer_value(&s["spanId"]).unwrap_or(-1);
        let key = key_of(seg, sp);
        if index.insert(key.clone(), i).is_some() {
            ambiguous.insert(key);
        }
    }
    for key in ambiguous {
        index.remove(&key);
    }
    let mut notes = vec![String::new(); spans.len()];
    // 每个 span 的父 index（找不到 = 根）。
    let mut parent: Vec<Option<usize>> = vec![None; spans.len()];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); spans.len()];
    for (i, s) in spans.iter().enumerate() {
        let seg = s.get("segmentId").and_then(|v| v.as_str()).unwrap_or("");
        let parent_span = integer_value(&s["parentSpanId"]).unwrap_or(-1);
        // 多个 refs 保留关联事实，不把第一个武断认作唯一父节点。
        let mut p: Option<usize> = None;
        let refs = s
            .get("refs")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if refs.len() == 1 {
            let r = &refs[0];
            p = index
                .get(&key_of(
                    &str_arg(r, "parentSegmentId"),
                    span_i64(r, "parentSpanId").unwrap_or(-1),
                ))
                .copied();
            if p.is_none() {
                notes[i] = "父关系未解析（引用缺失或身份重复）".into();
            }
        } else if refs.len() > 1 {
            notes[i] = "多个 refs：关联已列出，唯一父关系未确定".into();
        } else if parent_span >= 0 {
            p = index.get(&key_of(seg, parent_span)).copied();
            if p.is_none() {
                notes[i] = "父关系未解析（父节点缺失或身份重复）".into();
            }
        }
        if p == Some(i) {
            p = None;
            notes[i] = "父关系异常：自引用".into();
        }
        parent[i] = p;
        if let Some(pi) = p {
            children[pi].push(i);
        }
    }
    let start_of = |i: usize| integer_value(&spans[i]["startTime"]).unwrap_or(0);
    // 根集合：无父的 span，按开始时间排序。
    let mut roots: Vec<usize> = (0..spans.len()).filter(|i| parent[*i].is_none()).collect();
    roots.sort_by_key(|i| start_of(*i));
    for kids in children.iter_mut() {
        kids.sort_by_key(|i| start_of(*i));
    }
    // 前序遍历（显式栈，防极端深度递归）。
    let mut lines = Vec::new();
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|&i| (i, 0usize)).collect();
    let mut visited = vec![false; spans.len()];
    while let Some((i, depth)) = stack.pop() {
        if visited[i] {
            continue;
        }
        visited[i] = true;
        let mut line = render_span(&spans[i], depth);
        if !notes[i].is_empty() {
            line.push_str(&format!("{}  [{}]\n", "  ".repeat(depth), notes[i]));
        }
        lines.push(line);
        // 子节点逆序入栈，保证前序 + 时间升序输出。
        for &c in children[i].iter().rev() {
            if !visited[c] {
                stack.push((c, depth + 1));
            }
        }
    }
    // 兜底：环引用导致未访问到的 span 也补在后面，不吞数据。
    for (i, v) in visited.iter().enumerate() {
        if !v {
            lines.push(format!(
                "{}  [父关系未解析：环引用或连接到环]\n",
                render_span(&spans[i], 0)
            ));
        }
    }
    lines
}

/// 渲染单个 span 一行（含缩进）；出错 span 额外带 tags/logs 明细。
fn render_span(span: &Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let svc = span
        .get("serviceCode")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let ep = span
        .get("endpointName")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let is_err = span
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let start = integer_value(&span["startTime"]).unwrap_or(0);
    let end = integer_value(&span["endTime"]).unwrap_or(0);
    let cost = end.saturating_sub(start).max(0);
    let flag = if is_err { "❌ERROR" } else { "ok" };
    let mut line = format!("{indent}- {flag} {svc} · {ep} ({cost}ms)\n");
    if let Some(refs) = span["refs"].as_array().filter(|refs| refs.len() > 1) {
        for r in refs {
            line.push_str(&format!(
                "{indent}    ref={} : {} [{}]\n",
                str_arg(r, "parentSegmentId"),
                span_i64(r, "parentSpanId").unwrap_or(-1),
                str_arg(r, "type")
            ));
        }
    }
    if is_err {
        // 出错 span 带出 tags（含 http.status_code / error 等）。
        if let Some(tags) = span.get("tags").and_then(|t| t.as_array()) {
            for kv in tags.iter() {
                let k = kv.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let v = kv.get("value").and_then(|v| v.as_str()).unwrap_or("");
                if matches!(
                    k,
                    "status_code"
                        | "http.status_code"
                        | "error"
                        | "db.statement"
                        | "rpc.status_code"
                ) && !v.trim().is_empty()
                {
                    let v: String = v.chars().take(200).collect();
                    line.push_str(&format!("{indent}    {k}: {v}\n"));
                }
            }
        }
        if let Some(logs) = span.get("logs").and_then(|l| l.as_array()) {
            for entry in logs.iter().take(3) {
                if let Some(data) = entry.get("data").and_then(|d| d.as_array()) {
                    for kv in data.iter().take(6) {
                        let k = kv.get("key").and_then(|v| v.as_str()).unwrap_or("");
                        let v = kv.get("value").and_then(|v| v.as_str()).unwrap_or("");
                        let v: String = v.chars().take(400).collect();
                        if !v.trim().is_empty() {
                            line.push_str(&format!("{indent}    {k}: {v}\n"));
                        }
                    }
                }
            }
        }
    }
    line
}

/// 指标诊断：定位「哪个端点在恶化」。先探测 OAP 有哪些指标名，再查端点延迟/吞吐 TopN 与实例清单。
/// 不把相关性表述为已证明的因果（与预设脚本口径一致）。
async fn query_metrics(addr: &str, header: &str, args: &Value) -> Result<String, String> {
    fetch_metrics_budgeted(addr, header, args, &QueryBudget::new()).await
}

pub async fn fetch_metrics_budgeted(
    addr: &str,
    header: &str,
    args: &Value,
    budget: &QueryBudget,
) -> Result<String, String> {
    let service = str_arg(args, "service");
    if service.is_empty() {
        return Err(
            "schema_error: 指标诊断需要服务名：请在 service 参数里给出要诊断的服务。".into(),
        );
    }
    let minutes = args
        .get("minutes")
        .and_then(|v| v.as_i64())
        .filter(|m| *m > 0 && *m <= 24 * 60)
        .unwrap_or(60);
    let duration = QueryWindow::from_args(args, 60)?.duration();

    // 探测可用指标名，所有请求与调用方共享预算。
    let available = list_metric_names(addr, header, budget).await?;
    let can = |name: &str| available.is_empty() || available.iter().any(|m| m == name);

    let window_label = if args.get("start").is_some() {
        format!("{} 至 {}", str_arg(args, "start"), str_arg(args, "end"))
    } else {
        format!("最近 {minutes} 分钟")
    };
    let mut out = format!("SkyWalking 指标诊断（service={service}，{window_label}）：\n");

    // 端点延迟 TopN
    if can("endpoint_resp_time") {
        match top_endpoints(
            addr,
            header,
            "endpoint_resp_time",
            &service,
            &duration,
            budget,
        )
        .await
        {
            Ok(rows) if !rows.is_empty() => {
                out.push_str("\n【端点响应时间 TopN（ms，降序）】\n");
                for (name, val) in rows.iter().take(METRICS_TOPN as usize) {
                    out.push_str(&format!("- {name}: {val}\n"));
                }
            }
            Ok(_) => out.push_str("\n【端点响应时间】无数据\n"),
            Err(e) => return Err(e),
        }
    } else {
        out.push_str("\n【端点响应时间】OAP 未提供 endpoint_resp_time 指标\n");
    }

    // 端点吞吐 TopN
    if can("endpoint_cpm") {
        match top_endpoints(addr, header, "endpoint_cpm", &service, &duration, budget).await {
            Ok(rows) if !rows.is_empty() => {
                out.push_str("\n【端点吞吐 TopN（cpm，降序）】\n");
                for (name, val) in rows.iter().take(METRICS_TOPN as usize) {
                    out.push_str(&format!("- {name}: {val}\n"));
                }
            }
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }

    // 实例清单
    let sid = resolve_service_id_budgeted(addr, header, &service, budget).await?;
    match list_instances(addr, header, &sid, &duration, budget).await {
        Ok(names) if !names.is_empty() => {
            out.push_str(&format!("\n【服务实例（{} 个）】\n", names.len()));
            for n in names.iter().take(30) {
                out.push_str(&format!("- {n}\n"));
            }
        }
        Ok(_) => out.push_str("\n【服务实例】无数据\n"),
        Err(e) => return Err(e),
    }

    out.push_str("\n注：以上为相关性证据（延迟/吞吐/实例），不等于已证明的根因。结合日志（传 keyword）与调用链（传 trace_id）定位。");
    Ok(out)
}

/// 探测指标名；错误原样传播，不把鉴权或结构失败当作空清单。
async fn list_metric_names(
    addr: &str,
    header: &str,
    budget: &QueryBudget,
) -> Result<Vec<String>, String> {
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_LIST_METRICS,
        json!({ "regex": "" }),
        budget,
    )
    .await?;
    let rows = body
        .pointer("/data/metrics")
        .and_then(Value::as_array)
        .ok_or("schema_error: data.metrics must be an array")?;
    rows.iter()
        .map(|m| {
            m.get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "schema_error: metric.name must be a string".into())
        })
        .collect()
}

/// 端点 TopN：返回 (端点名, 值)，按值降序。
async fn top_endpoints(
    addr: &str,
    header: &str,
    metric: &str,
    service: &str,
    duration: &Value,
    budget: &QueryBudget,
) -> Result<Vec<(String, f64)>, String> {
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_SORT_METRICS,
        json!({
            "condition": {
                "name": metric,
                "parentService": service,
                "normal": true,
                "scope": "Endpoint",
                "topN": METRICS_TOPN,
                "order": "DES",
            },
            "duration": duration,
        }),
        budget,
    )
    .await?;
    let rows = body
        .pointer("/data/records")
        .and_then(Value::as_array)
        .ok_or("schema_error: data.records must be an array")?;
    rows.iter()
        .map(|r| {
            let name = r
                .get("name")
                .and_then(Value::as_str)
                .ok_or("schema_error: record.name must be a string")?
                .to_string();
            let val = r
                .get("value")
                .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
                .filter(|v| v.is_finite())
                .ok_or("schema_error: record.value must be finite numeric")?;
            Ok((name, val))
        })
        .collect()
}

/// 实例清单：返回实例名。
async fn list_instances(
    addr: &str,
    header: &str,
    service_id: &str,
    duration: &Value,
    budget: &QueryBudget,
) -> Result<Vec<String>, String> {
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_INSTANCES,
        json!({ "serviceId": service_id, "duration": duration }),
        budget,
    )
    .await?;
    let rows = body
        .pointer("/data/instances")
        .and_then(Value::as_array)
        .ok_or("schema_error: data.instances must be an array")?;
    rows.iter()
        .map(|r| {
            r.get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "schema_error: instance.name must be a string".into())
        })
        .collect()
}

/// 每次查询入口捕获一次 now；调用方跨页传同一 start/end 可固定整个分页窗口。
#[derive(Debug)]
struct QueryWindow {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
}

impl QueryWindow {
    fn from_args(args: &Value, default_minutes: i64) -> Result<Self, String> {
        Self::at(args, default_minutes, Utc::now())
    }

    fn at(args: &Value, default_minutes: i64, now: DateTime<Utc>) -> Result<Self, String> {
        let offset = match args.get("timezone_offset_minutes") {
            Some(v) => {
                let minutes = v.as_i64().filter(|v| (-1439..=1439).contains(v)).ok_or(
                    "schema_error: timezone_offset_minutes must be an integer within -1439..1439",
                )?;
                FixedOffset::east_opt(minutes as i32 * 60)
                    .ok_or("schema_error: invalid OAP offset")?
            }
            None => *now.with_timezone(&Local).offset(),
        };
        let (start, end) = match (args.get("start"), args.get("end")) {
            (None, None) => {
                let minutes = args
                    .get("minutes")
                    .and_then(Value::as_i64)
                    .filter(|m| *m > 0 && *m <= 24 * 60)
                    .unwrap_or(default_minutes);
                let end = now.with_timezone(&offset);
                (end - chrono::Duration::minutes(minutes), end)
            }
            (Some(start), Some(end)) => {
                let parse = |v: &Value| -> Result<DateTime<FixedOffset>, String> {
                    let raw = v
                        .as_str()
                        .ok_or("schema_error: start/end must be RFC3339 strings")?;
                    DateTime::parse_from_rfc3339(raw)
                        .map(|dt| dt.with_timezone(&offset))
                        .map_err(|_| "schema_error: start/end must include RFC3339 timezone".into())
                };
                (parse(start)?, parse(end)?)
            }
            _ => return Err("schema_error: start and end must be supplied together".into()),
        };
        if start > end {
            return Err("schema_error: start must not be after end".into());
        }
        Ok(Self { start, end })
    }

    fn duration(&self) -> Value {
        // SECOND 保留明确时间的分秒，包括大于 24h 的窗口。毫秒在本地精确筛选。
        json!({ "start": self.start.format("%Y-%m-%d %H%M%S").to_string(),
            "end": self.end.format("%Y-%m-%d %H%M%S").to_string(), "step": "SECOND" })
    }
}

#[cfg(test)]
fn duration_window(minutes: i64) -> Value {
    QueryWindow::from_args(&json!({"minutes": minutes}), DEFAULT_WINDOW_MIN)
        .unwrap()
        .duration()
}

/// findService 的 HTTP/鉴权/结构失败原样传播；仅明确 null（服务不存在）用旧编码兜底。
async fn resolve_service_id_budgeted(
    addr: &str,
    header: &str,
    service: &str,
    budget: &QueryBudget,
) -> Result<String, String> {
    let body = graphql_budgeted(
        addr,
        header,
        QUERY_FIND_SERVICE,
        json!({ "serviceName": service }),
        budget,
    )
    .await?;
    let found = body
        .pointer("/data/service")
        .ok_or("schema_error: data.service missing")?;
    if found.is_null() {
        return Ok(service_id(service));
    }
    found
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "schema_error: data.service.id must be a nonempty string".into())
}

/// SkyWalking serviceId 确定性编码：`base64(服务名) + "." + layer`，GENERAL 服务 layer=1。
/// （IDManager.ServiceID.buildId）仅作 findService 失败时的兜底。
fn service_id(service: &str) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(service.as_bytes());
    format!("{b64}.1")
}

/// SkyWalking endpointId 编码：`serviceId_base64(endpoint名)`。
/// （IDManager.EndpointID.buildId：serviceId + "_" + base64(endpointName)）
fn endpoint_id(service_id: &str, endpoint: &str) -> String {
    use base64::Engine;
    let ep_b64 = base64::engine::general_purpose::STANDARD.encode(endpoint.as_bytes());
    format!("{service_id}_{ep_b64}")
}

fn str_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn none_if_empty(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

/// 把 header 配置拆成多对：按换行 / 分号分隔多条 `Key: Value`，鉴权版 OAP 常需多个 header。
/// 单条格式不符的行跳过（不整体失败）。
fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(['\n', ';']).filter_map(parse_header).collect()
}

/// 把 `Key: Value` 形式的单行 header 拆成一对；格式不符返回 None。
fn parse_header(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (k, v) = raw.split_once(':')?;
    let (k, v) = (k.trim(), v.trim());
    if k.is_empty() || v.is_empty() {
        return None;
    }
    Some((k.to_string(), v.to_string()))
}

#[cfg(test)]
#[path = "wecom_skywalking_tests.rs"]
mod protocol_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn row(level: &str, content: &str, trace: &str) -> LogRow {
        LogRow {
            ts: 0,
            service: "svc".into(),
            instance: "instance-1".into(),
            endpoint: "ep".into(),
            level: level.into(),
            thread: String::new(),
            logger: String::new(),
            trace_id: trace.into(),
            content: content.into(),
        }
    }

    #[test]
    fn is_error_row_by_level_or_content() {
        assert!(is_error_row(&row("ERROR", "boom", "")));
        assert!(is_error_row(&row("info", "NullPointerException at x", "")));
        assert!(is_error_row(&row("INFO", "下单异常", "")));
        assert!(!is_error_row(&row("INFO", "ok done", "")));
        assert!(!is_error_row(&row("DEBUG", "正常", "")));
    }

    #[test]
    fn summarize_logs_counts_errors_and_prioritizes_error_traces() {
        let rows = vec![
            row("INFO", "ok", "t-info"),
            row("ERROR", "boom", "t-err1"),
            row("INFO", "ok2", "t-info"), // 去重
            row("ERROR", "异常", "t-err2"),
        ];
        let (err, ids) = summarize_logs(&rows);
        assert_eq!(err, 2);
        // 错误 traceId 排前，普通在后，去重
        assert_eq!(ids, vec!["t-err1", "t-err2", "t-info"]);
        assert_eq!(first_trace_id(&rows).as_deref(), Some("t-err1"));
    }

    #[test]
    fn fmt_ts_handles_zero_and_valid() {
        assert_eq!(fmt_ts(0), "--");
        // 非零时间戳应格式化成 "MM-DD HH:MM:SS"（长度 14）
        assert_eq!(fmt_ts(1_700_000_000_000).len(), 14);
    }

    #[test]
    fn format_logs_shows_summary_and_level_prefix() {
        let rows = vec![row("ERROR", "boom", "t1"), row("INFO", "ok", "")];
        let out = format_logs(&rows, 30, 1, &json!({ "keyword": "boom" }));
        assert!(out.contains("摘要：错误级 1 条"), "{out}");
        assert!(out.contains("traceId"), "{out}");
        assert!(out.contains("[ERROR]"), "{out}");
    }

    #[test]
    fn format_rows_brief_caps_and_summarizes() {
        let rows: Vec<LogRow> = (0..25).map(|i| row("INFO", &format!("l{i}"), "")).collect();
        let brief = format_rows_brief(&rows);
        assert!(brief.contains("另有 5 条未展示"), "{brief}");
        assert_eq!(format_rows_brief(&[]), "（无）");
    }

    #[test]
    fn parse_keywords_from_array_or_comma() {
        // 数组优先
        let a = json!({ "keywords": ["订单123456", "error"] });
        assert_eq!(parse_keywords(&a), vec!["订单123456", "error"]);
        // 单值逗号拆分（中英文逗号）
        let b = json!({ "keyword": "2024102812345, error，timeout" });
        assert_eq!(
            parse_keywords(&b),
            vec!["2024102812345", "error", "timeout"]
        );
        // 单词
        let c = json!({ "keyword": "NullPointer" });
        assert_eq!(parse_keywords(&c), vec!["NullPointer"]);
        // 空
        assert!(parse_keywords(&json!({})).is_empty());
    }

    #[test]
    fn build_condition_puts_all_keywords_in_array() {
        let kws = vec!["2024102812345".to_string(), "error".to_string()];
        let cond = build_log_condition("", "", &kws, "", "", "", &duration_window(60), 1, 40);
        let arr = cond
            .get("keywordsOfContent")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("2024102812345"));
        assert_eq!(arr[1].as_str(), Some("error"));
        // traceId 模式互斥：不带 keywordsOfContent
        let t = build_log_condition("", "", &kws, "", "", "tid", &duration_window(60), 1, 40);
        assert!(t.get("keywordsOfContent").is_none());
        assert!(t.get("relatedTrace").is_some());
    }

    #[test]
    fn header_parsing_tolerates_spaces_and_rejects_garbage() {
        assert_eq!(
            parse_header("Authorization: Bearer abc"),
            Some(("Authorization".into(), "Bearer abc".into()))
        );
        assert_eq!(
            parse_header("  X-Token:t  "),
            Some(("X-Token".into(), "t".into()))
        );
        assert_eq!(parse_header(""), None);
        assert_eq!(parse_header("nocolon"), None);
        assert_eq!(parse_header(": v"), None);
        assert_eq!(parse_header("k:"), None);
    }

    #[test]
    fn multi_header_parsing_splits_on_newline_and_semicolon() {
        let hs = parse_headers("Authorization: Bearer abc\nX-Tenant: t1 ; X-Trace: on");
        assert_eq!(hs.len(), 3);
        assert_eq!(hs[0], ("Authorization".into(), "Bearer abc".into()));
        assert_eq!(hs[1], ("X-Tenant".into(), "t1".into()));
        assert_eq!(hs[2], ("X-Trace".into(), "on".into()));
        // 空与坏行跳过，不整体失败
        let hs2 = parse_headers("\n;  ; Good: v ;garbage;");
        assert_eq!(hs2, vec![("Good".into(), "v".into())]);
        assert!(parse_headers("").is_empty());
    }

    #[test]
    fn span_tree_nests_by_refs_and_parent_span() {
        // root(seg A, span 0) → child same-seg(span 1) → cross-seg(seg B, span 0 refs A:1)
        let spans = json!([
            { "segmentId": "A", "spanId": 1, "parentSpanId": 0, "serviceCode": "svc-a", "endpointName": "/mid", "startTime": 20, "endTime": 60, "isError": false },
            { "segmentId": "A", "spanId": 0, "parentSpanId": -1, "serviceCode": "svc-a", "endpointName": "/root", "startTime": 10, "endTime": 100, "isError": false },
            { "segmentId": "B", "spanId": 0, "parentSpanId": -1, "refs": [{ "parentSegmentId": "A", "parentSpanId": 1, "type": "CROSS_PROCESS" }], "serviceCode": "svc-b", "endpointName": "/leaf", "startTime": 30, "endTime": 50, "isError": true, "tags": [{ "key": "status_code", "value": "500" }] }
        ]);
        let arr = spans.as_array().unwrap();
        let lines = build_span_tree(arr);
        // 3 个 span 都在，且根 /root 不缩进、/mid 1 级、/leaf 2 级
        let joined = lines.join("");
        assert!(joined.contains("/root"), "{joined}");
        let root_line = lines.iter().find(|l| l.contains("/root")).unwrap();
        let mid_line = lines.iter().find(|l| l.contains("/mid")).unwrap();
        let leaf_line = lines.iter().find(|l| l.contains("/leaf")).unwrap();
        assert!(root_line.starts_with("- "), "root 不缩进：{root_line}");
        assert!(mid_line.starts_with("  - "), "mid 缩进 1 级：{mid_line}");
        assert!(
            leaf_line.starts_with("    - "),
            "leaf 缩进 2 级：{leaf_line}"
        );
        // 出错 span 带出 status_code
        assert!(
            leaf_line.contains("500"),
            "出错 span 应带 status_code：{leaf_line}"
        );
        assert!(leaf_line.contains("❌ERROR"));
    }

    #[test]
    fn span_tree_handles_cycle_without_losing_spans() {
        // 两个 span 互相指对方为父（异常数据）：不能死循环，且都要输出。
        let spans = json!([
            { "segmentId": "A", "spanId": 0, "parentSpanId": 1, "serviceCode": "a", "endpointName": "/x", "startTime": 1, "endTime": 2 },
            { "segmentId": "A", "spanId": 1, "parentSpanId": 0, "serviceCode": "a", "endpointName": "/y", "startTime": 1, "endTime": 2 }
        ]);
        let lines = build_span_tree(spans.as_array().unwrap());
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn duration_window_second_step_under_24h() {
        let w = duration_window(240);
        assert_eq!(w.get("step").and_then(|v| v.as_str()), Some("SECOND"));
        let start = w.get("start").and_then(|v| v.as_str()).unwrap();
        let end = w.get("end").and_then(|v| v.as_str()).unwrap();
        // 形如 "2026-10-28 231205"：日期 10 + 空格 + 6 位 HHmmss
        assert_eq!(start.len(), 17, "start={start}");
        assert_eq!(end.len(), 17, "end={end}");
        assert!(start <= end, "start {start} 不应晚于 end {end}");
    }

    #[test]
    fn duration_window_keeps_seconds_for_long_window() {
        let w = duration_window(24 * 60);
        assert_eq!(w.get("step").and_then(|v| v.as_str()), Some("SECOND"));
        assert_eq!(w.get("start").and_then(|v| v.as_str()).unwrap().len(), 17);
    }

    #[test]
    fn empty_addr_is_actionable() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(run("  ", "", &json!({ "keyword": "x" })))
            .unwrap_err();
        assert!(err.contains("未配置 SkyWalking"), "{err}");
    }

    #[test]
    fn service_id_encoding_matches_oap_rule() {
        // base64("pmys.saas.match.service") + ".1"
        assert_eq!(
            service_id("pmys.saas.match.service"),
            "cG15cy5zYWFzLm1hdGNoLnNlcnZpY2U=.1"
        );
    }

    #[test]
    fn endpoint_id_joins_service_id_and_endpoint_b64() {
        let sid = "cG15cy5zYWFzLm1hdGNoLnNlcnZpY2U=.1";
        // serviceId + "_" + base64("/api/x")
        assert_eq!(endpoint_id(sid, "/api/x"), format!("{sid}_L2FwaS94"));
    }
}
