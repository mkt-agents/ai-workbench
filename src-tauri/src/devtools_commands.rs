//! DevTools backend: HTTP client.
//!
//! Windows-only (matches the rest of the app). Uses the already available
//! `reqwest` crate for the HTTP client.

use serde::{Deserialize, Serialize};
use std::time::Instant;

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
