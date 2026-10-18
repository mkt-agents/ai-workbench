use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{
    ErrorCode, GENERATE_CONNECT_TIMEOUT_SECS, GENERATE_STREAM_IDLE_SECS, GENERATE_TEXT_TIMEOUT_SECS,
    MODEL_LIST_TIMEOUT_SECS, MODEL_TEST_TIMEOUT_SECS,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AIModelConfig {
    pub id: Option<String>,
    pub name: String,
    pub provider: String,
    pub api_key: String,
    #[serde(default)]
    pub auth_type: Option<String>,
    pub base_url: String,
    pub model: String,
    pub temperature: f64,
    pub max_tokens: i64,
    pub is_default: bool,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConnectionResult {
    pub success: bool,
    pub message: String,
}

#[tauri::command]
pub async fn test_model_connection(config: AIModelConfig) -> Result<ModelConnectionResult, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(MODEL_TEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let base = trim_trailing_slash(&config.base_url);
    let provider = config.provider.to_lowercase();

    let (url, body, use_anthropic) = if provider == "anthropic" {
        let url = if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        };
        let body = serde_json::json!({
            "model": config.model,
            "max_tokens": 5,
            "messages": [{ "role": "user", "content": "Hello" }],
        });
        (url, body, true)
    } else {
        // OpenAI-compatible (incl. Ollama with /v1 base)
        let url = format!("{}/chat/completions", base);
        let body = serde_json::json!({
            "model": config.model,
            "messages": [{ "role": "user", "content": "Hello" }],
            "max_tokens": 5,
        });
        (url, body, false)
    };

    let mut req = client.post(&url).json(&body);

    if use_anthropic {
        if !config.api_key.is_empty() {
            req = req
                .header("x-api-key", &config.api_key)
                .header("anthropic-version", "2023-06-01");
        }
    } else if !config.api_key.is_empty() {
        if provider == "mimo" {
            req = req.header("api-key", &config.api_key);
        } else {
            // token_plan and api both use Bearer (aligned with DSH)
            req = req.header("authorization", format!("Bearer {}", config.api_key));
        }
    }
    // Ollama: no key is fine

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                Ok(ModelConnectionResult {
                    success: true,
                    message: format!("Connection successful (HTTP {})", status),
                })
            } else {
                let body = resp.text().await.unwrap_or_default();
                Ok(ModelConnectionResult {
                    success: false,
                    message: format!(
                        "HTTP {}: {}",
                        status,
                        body.chars().take(200).collect::<String>()
                    ),
                })
            }
        }
        Err(e) => Ok(ModelConnectionResult {
            success: false,
            message: format!("Connection failed: {}", e),
        }),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsResult {
    pub success: bool,
    pub models: Vec<ListedModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsRequest {
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    pub base_url: String,
}

/// List models via OpenAI-compatible GET {base}/models.
#[tauri::command]
pub async fn list_provider_models(config: ListModelsRequest) -> Result<ListModelsResult, String> {
    let provider = config.provider.to_lowercase();
    if provider == "anthropic" {
        return Ok(ListModelsResult {
            success: false,
            models: vec![],
            message: Some("This provider does not support model list API".to_string()),
        });
    }

    let base = trim_trailing_slash(&config.base_url);
    if base.is_empty() {
        return Ok(ListModelsResult {
            success: false,
            models: vec![],
            message: Some("Base URL is required".to_string()),
        });
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(MODEL_LIST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let url = format!("{}/models", base);
    let mut req = client.get(&url);

    if !config.api_key.is_empty() {
        if provider == "mimo" {
            req = req.header("api-key", &config.api_key);
        } else {
            req = req.header("authorization", format!("Bearer {}", config.api_key));
        }
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Ok(ListModelsResult {
                    success: false,
                    models: vec![],
                    message: Some(format!(
                        "HTTP {}: {}",
                        status,
                        text.chars().take(200).collect::<String>()
                    )),
                });
            }

            let models = parse_openai_models_response(&text);
            Ok(ListModelsResult {
                success: true,
                models,
                message: None,
            })
        }
        Err(e) => Ok(ListModelsResult {
            success: false,
            models: vec![],
            message: Some(format!("Request failed: {}", e)),
        }),
    }
}

fn parse_openai_models_response(text: &str) -> Vec<ListedModel> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };

    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .or_else(|| value.as_array().cloned())
        .unwrap_or_default();

    let mut models: Vec<ListedModel> = data
        .into_iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("name").and_then(|v| v.as_str()))?
                .trim()
                .to_string();
            if id.is_empty() {
                return None;
            }
            Some(ListedModel { id, label: None })
        })
        .collect();

    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

pub(crate) fn trim_trailing_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateTextRequest {
    pub config: AIModelConfig,
    pub system: String,
    pub user: String,
}

pub(crate) fn apply_auth_headers(
    mut req: reqwest::RequestBuilder,
    config: &AIModelConfig,
    use_anthropic: bool,
) -> reqwest::RequestBuilder {
    let provider = config.provider.to_lowercase();
    if use_anthropic {
        if !config.api_key.is_empty() {
            req = req
                .header("x-api-key", &config.api_key)
                .header("anthropic-version", "2023-06-01");
        }
    } else if !config.api_key.is_empty() {
        if provider == "mimo" {
            req = req.header("api-key", &config.api_key);
        } else {
            req = req.header("authorization", format!("Bearer {}", config.api_key));
        }
    }
    req
}

fn extract_content_parts(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let t = s.trim();
        return if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        };
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str() {
            let t = s.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
            continue;
        }
        let typ = item.get("type").and_then(|t| t.as_str()).unwrap_or("text");
        if typ == "text" || typ == "output_text" {
            if let Some(text) = item
                .get("text")
                .and_then(|t| t.as_str())
                .or_else(|| item.get("content").and_then(|t| t.as_str()))
            {
                let t = text.trim();
                if !t.is_empty() {
                    parts.push(t.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// 推理模型（DeepSeek-R1、QwQ 等）会把思考过程用 `<think>…</think>` 混进正文；有的服务商
/// 把思考挪进 reasoning 字段，正文开头只剩一个落单的 `</think>`。两种都不是回答，剥掉。
/// 前端 `aiHub/thinking.ts` 的 `stripThinking` 是同一规则（流式分块只能在前端拼完再剥）。
pub(crate) fn strip_think_tags(text: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    // 只做 ASCII 小写，字节偏移与原文一一对应
    let lower = text.to_ascii_lowercase();
    if !lower.contains(OPEN) && !lower.contains(CLOSE) {
        return text.to_string();
    }
    let mut i = 0;
    if let Some(close) = lower.find(CLOSE) {
        if lower.find(OPEN).map_or(true, |open| close < open) {
            i = close + CLOSE.len();
        }
    }
    let mut out = String::with_capacity(text.len());
    while let Some(rel) = lower[i..].find(OPEN) {
        let open = i + rel;
        out.push_str(&text[i..open]);
        match lower[open..].find(CLOSE) {
            Some(rel_close) => i = open + rel_close + CLOSE.len(),
            // 没闭合：后面全是思考（被截断或还没想完）
            None => return out.trim_start().to_string(),
        }
    }
    out.push_str(&text[i..]);
    out.trim_start().to_string()
}

fn extract_openai_text(json: &serde_json::Value) -> Option<String> {
    let choice = json.get("choices")?.as_array()?.first()?;
    if let Some(msg) = choice.get("message") {
        if let Some(content) = msg.get("content") {
            if let Some(text) = extract_content_parts(content) {
                return Some(text);
            }
        }
        // Some providers put the answer in message.reasoning / refusal-adjacent fields
        if let Some(text) = msg
            .get("reasoning_content")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(text.to_string());
        }
    }
    // Legacy /completions-style
    if let Some(text) = choice.get("text").and_then(|t| t.as_str()) {
        let t = text.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    None
}

fn extract_anthropic_text(json: &serde_json::Value) -> Option<String> {
    let content = json.get("content")?.as_array()?;
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                let t = text.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn describe_empty_generation(json: &serde_json::Value, raw: &str) -> String {
    let finish = json
        .pointer("/choices/0/finish_reason")
        .or_else(|| json.get("stop_reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let snippet: String = raw.chars().take(180).collect();
    format!("模型未返回有效文本 (finish_reason={finish}): {snippet}")
}

/// One shared client for one-shot generations. Building one per call threw away
/// the connection pool, so every call — and every map brief of a report — paid a
/// fresh TCP/TLS handshake before the model even saw the prompt.
fn text_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(GENERATE_TEXT_TIMEOUT_SECS))
            .build()
            .expect("failed to build HTTP client")
    })
}

/// One-shot text generation via the configured model (OpenAI-compatible or Anthropic).
#[tauri::command]
pub async fn generate_text(req: GenerateTextRequest) -> Result<String, String> {
    if req.config.base_url.trim().is_empty() || req.config.model.trim().is_empty() {
        let msg = ErrorCode::AiModelNotConfigured.as_str().to_string() + " 模型 Base URL 或模型名未配置";
        return Err(msg);
    }
    if req.user.trim().is_empty() {
        let msg = ErrorCode::AiEmptyPrompt.as_str().to_string() + " 生成内容为空";
        return Err(msg);
    }

    let client = text_client();

    let base = trim_trailing_slash(&req.config.base_url);
    let provider = req.config.provider.to_lowercase();
    let max_tokens = {
        let m = if req.config.max_tokens > 0 {
            req.config.max_tokens
        } else {
            200
        };
        m.clamp(32, 4096)
    };

    let (url, body, use_anthropic) = if provider == "anthropic" {
        let url = if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        };
        let body = serde_json::json!({
            "model": req.config.model,
            "max_tokens": max_tokens,
            "system": req.system,
            "messages": [{ "role": "user", "content": req.user }],
        });
        (url, body, true)
    } else {
        let url = format!("{}/chat/completions", base);
        let mut messages = Vec::new();
        if !req.system.trim().is_empty() {
            messages.push(serde_json::json!({ "role": "system", "content": req.system }));
        }
        messages.push(serde_json::json!({ "role": "user", "content": req.user }));
        let body = serde_json::json!({
            "model": req.config.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": req.config.temperature.clamp(0.0, 1.0),
        });
        (url, body, false)
    };

    let http_req = apply_auth_headers(client.post(&url).json(&body), &req.config, use_anthropic);
    let resp = http_req
        .send()
        .await
        .map_err(|e| format!("请求模型失败: {}", e))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "模型返回 HTTP {}: {}",
            status,
            text.chars().take(240).collect::<String>()
        ));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析模型响应失败: {}", e))?;
    let content = if use_anthropic {
        extract_anthropic_text(&json)
    } else {
        extract_openai_text(&json)
    }
    .map(|t| strip_think_tags(&t))
    .filter(|t| !t.trim().is_empty())
    .ok_or_else(|| describe_empty_generation(&json, &text))?;

    Ok(content)
}

/// One previous Q&A turn for multi-turn requests (AI Hub follow-ups).
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateTextStreamRequest {
    pub config: AIModelConfig,
    pub system: String,
    pub user: String,
    pub request_id: String,
    /// Earlier turns (oldest first, alternating user/assistant). Optional so
    /// existing single-shot callers keep working unchanged.
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamChunkPayload {
    id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamDonePayload {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_tokens: Option<u32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamErrorPayload {
    id: String,
    error: String,
}

fn openai_delta_text(json: &serde_json::Value) -> Option<String> {
    let choice = json.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    if let Some(content) = delta.get("content") {
        if let Some(s) = content.as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn anthropic_delta_text(json: &serde_json::Value) -> Option<String> {
    let typ = json.get("type")?.as_str()?;
    if typ != "content_block_delta" {
        return None;
    }
    let delta = json.get("delta")?;
    if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
        if let Some(s) = delta.get("text").and_then(|t| t.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Streaming text generation; emits `generate-text-chunk` / `generate-text-done` / `generate-text-error`.
#[tauri::command]
pub async fn generate_text_stream(
    app: tauri::AppHandle,
    req: GenerateTextStreamRequest,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tauri::Emitter;

    let request_id = req.request_id.clone();
    let _guard = crate::cancellation::CancelGuard::new(&request_id);
    let emit_err = |app: &tauri::AppHandle, msg: String| {
        let _ = app.emit(
            "generate-text-error",
            StreamErrorPayload {
                id: request_id.clone(),
                error: msg,
            },
        );
    };

    if req.config.base_url.trim().is_empty() || req.config.model.trim().is_empty() {
        let e = format!("[{}] 模型 Base URL 或模型名未配置", ErrorCode::AiModelNotConfigured.as_str());
        emit_err(&app, e.clone());
        return Err(e);
    }
    if req.user.trim().is_empty() {
        let e = format!("[{}] 生成内容为空", ErrorCode::AiEmptyPrompt.as_str());
        emit_err(&app, e.clone());
        return Err(e);
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(GENERATE_CONNECT_TIMEOUT_SECS))
        .read_timeout(Duration::from_secs(GENERATE_STREAM_IDLE_SECS))
        .build()
        .map_err(|e| {
            let msg = format!("Failed to create HTTP client: {}", e);
            emit_err(&app, msg.clone());
            msg
        })?;

    let base = trim_trailing_slash(&req.config.base_url);
    let provider = req.config.provider.to_lowercase();
    let max_tokens = {
        let m = if req.config.max_tokens > 0 {
            req.config.max_tokens
        } else {
            200
        };
        m.clamp(32, 4096)
    };

    let history_messages: Vec<serde_json::Value> = req
        .history
        .iter()
        .map(|turn| {
            serde_json::json!({
                "role": if turn.role == "assistant" { "assistant" } else { "user" },
                "content": turn.content,
            })
        })
        .collect();

    let (url, body, use_anthropic) = if provider == "anthropic" {
        let url = if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        };
        let mut messages = history_messages;
        messages.push(serde_json::json!({ "role": "user", "content": req.user }));
        let body = serde_json::json!({
            "model": req.config.model,
            "max_tokens": max_tokens,
            "stream": true,
            "system": req.system,
            "messages": messages,
        });
        (url, body, true)
    } else {
        let url = format!("{}/chat/completions", base);
        let mut messages = Vec::new();
        if !req.system.trim().is_empty() {
            messages.push(serde_json::json!({ "role": "system", "content": req.system }));
        }
        messages.extend(history_messages);
        messages.push(serde_json::json!({ "role": "user", "content": req.user }));
        let body = serde_json::json!({
            "model": req.config.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": req.config.temperature.clamp(0.0, 1.0),
            "stream": true,
            // Ask OpenAI-compatible providers for a final usage chunk when supported.
            "stream_options": { "include_usage": true },
        });
        (url, body, false)
    };

    let started = std::time::Instant::now();
    let http_req = apply_auth_headers(client.post(&url).json(&body), &req.config, use_anthropic);
    let resp = match http_req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("请求模型失败: {}", e);
            emit_err(&app, msg.clone());
            return Err(msg);
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let msg = format!(
            "模型返回 HTTP {}: {}",
            status,
            text.chars().take(240).collect::<String>()
        );
        emit_err(&app, msg.clone());
        return Err(msg);
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut saw_text = false;
    let mut prompt_tokens: Option<u32> = None;
    let mut completion_tokens: Option<u32> = None;

    let mut consume_line = |line_raw: &str| {
        let line = line_raw.trim();
        if line.is_empty() {
            return;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        if let Some(usage) = json.get("usage") {
            if let Some(n) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                prompt_tokens = Some(n as u32);
            }
            if let Some(n) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                completion_tokens = Some(n as u32);
            }
            // Anthropic-style keys
            if prompt_tokens.is_none() {
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    prompt_tokens = Some(n as u32);
                }
            }
            if completion_tokens.is_none() {
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    completion_tokens = Some(n as u32);
                }
            }
        }
        let piece = if use_anthropic {
            anthropic_delta_text(&json)
        } else {
            openai_delta_text(&json)
        };
        if let Some(text) = piece {
            saw_text = true;
            let _ = app.emit(
                "generate-text-chunk",
                StreamChunkPayload {
                    id: request_id.clone(),
                    text,
                },
            );
        }
    };

    while let Some(item) = stream.next().await {
        // Check for cancellation before processing each chunk.
        if crate::cancellation::is_cancelled(&request_id) {
            let _ = app.emit(
                "generate-text-error",
                StreamErrorPayload {
                    id: request_id.clone(),
                    error: format!("[{}] 已取消", ErrorCode::AiRequestCancelled.as_str()),
                },
            );
            return Err(format!("[{}] 已取消", ErrorCode::AiRequestCancelled.as_str()));
        }
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("读取流失败: {}", e);
                emit_err(&app, msg.clone());
                return Err(msg);
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buffer.find('\n') {
            let mut line = buffer[..pos].to_string();
            buffer = buffer[pos + 1..].to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            consume_line(&line);
        }
    }

    // Flush trailing SSE line without a final newline
    if !buffer.trim().is_empty() {
        consume_line(&buffer);
        buffer.clear();
    }

    if !saw_text {
        let msg = format!("[{}] 模型未返回有效文本", ErrorCode::AiEmptyResponse.as_str());
        emit_err(&app, msg.clone());
        return Err(msg);
    }

    let _ = app.emit(
        "generate-text-done",
        StreamDonePayload {
            id: request_id.clone(),
            elapsed_ms: Some(started.elapsed().as_millis() as u64),
            prompt_tokens,
            completion_tokens,
        },
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAiEndpoint {
    pub kind: String,
    pub base_url: String,
    pub label: String,
    pub reachable: bool,
    pub models: Vec<String>,
}

/// Probe common local OpenAI-compatible servers (Ollama / LM Studio).
#[tauri::command]
pub async fn detect_local_ai_endpoints() -> Result<Vec<LocalAiEndpoint>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let candidates = [
        ("ollama", "http://127.0.0.1:11434/v1", "Ollama"),
        ("lmstudio", "http://127.0.0.1:1234/v1", "LM Studio"),
    ];

    let mut out = Vec::new();
    for (kind, base, label) in candidates {
        let models_url = format!("{}/models", base.trim_end_matches('/'));
        match client.get(&models_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let mut models = Vec::new();
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
                        for item in arr {
                            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                                models.push(id.to_string());
                            }
                        }
                    }
                }
                // Ollama native /api/tags fallback when /v1/models is empty
                if models.is_empty() && kind == "ollama" {
                    if let Ok(tags) = client
                        .get("http://127.0.0.1:11434/api/tags")
                        .send()
                        .await
                    {
                        if let Ok(json) = tags.json::<serde_json::Value>().await {
                            if let Some(arr) = json.get("models").and_then(|m| m.as_array()) {
                                for item in arr {
                                    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                        models.push(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                out.push(LocalAiEndpoint {
                    kind: kind.to_string(),
                    base_url: base.to_string(),
                    label: label.to_string(),
                    reachable: true,
                    models,
                });
            }
            _ => {
                out.push(LocalAiEndpoint {
                    kind: kind.to_string(),
                    base_url: base.to_string(),
                    label: label.to_string(),
                    reachable: false,
                    models: vec![],
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::strip_think_tags;

    #[test]
    fn strips_think_blocks_and_orphan_close_tags() {
        assert_eq!(strip_think_tags("<think>先想想</think>\n\n答案"), "答案");
        // 服务商把思考挪进 reasoning 字段，正文只剩落单的闭合标签
        assert_eq!(strip_think_tags("</think>\n"), "");
        assert_eq!(strip_think_tags("残留思考</think>\n答案"), "答案");
        // 没闭合 = 后面全是思考
        assert_eq!(strip_think_tags("答案<think>还在想"), "答案");
        assert_eq!(strip_think_tags("<THINK>x</THINK>好的"), "好的");
        assert_eq!(strip_think_tags("a<think>1</think>b<think>2</think>c"), "abc");
        // 普通文本原样返回
        assert_eq!(strip_think_tags("  缩进保留"), "  缩进保留");
    }
}
