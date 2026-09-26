//! End-to-end tests for the gateway proper: the real axum router is bound to
//! an ephemeral loopback port and driven over HTTP, so the address gate, the
//! key check and the audit log are exercised together rather than per-unit.

use super::*;

/// Bring up the router and hand back its base URL plus the DB it reads from.
async fn spawn(adapter_json: Option<&str>) -> (String, Arc<Mutex<Connection>>) {
    let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    crate::codebuddy::ensure_schema(&conn.lock().unwrap()).unwrap();
    {
        let guard = conn.lock().unwrap();
        wb_commands::update_settings(
            &guard,
            &WbSettingsPatch {
                adapter_json: adapter_json.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
        wb_keys::create_key(&guard, "test", now_millis()).unwrap();
    }
    let ctx = Ctx {
        conn: Arc::clone(&conn),
        pool: Arc::new(Mutex::new(pool::Pool::default())),
        lan_enabled: false,
    };
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = std_listener.local_addr().unwrap().port();
    std_listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
    let app = build_router(ctx).into_make_service_with_connect_info::<SocketAddr>();
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://127.0.0.1:{port}");
    wait_until_ready(&base).await;
    (base, conn)
}

async fn wait_until_ready(base: &str) {
    let client = reqwest::Client::new();
    for _ in 0..200 {
        if client.get(format!("{base}/healthz")).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("gateway never came up on {base}");
}

fn test_key(conn: &Arc<Mutex<Connection>>) -> String {
    conn.lock()
        .unwrap()
        .query_row("SELECT key FROM wb_api_keys LIMIT 1", [], |r| r.get(0))
        .unwrap()
}

/// (status, key_id, error) per log row, oldest first.
fn log_rows(conn: &Arc<Mutex<Connection>>) -> Vec<(i64, Option<i64>, Option<String>)> {
    conn.lock()
        .unwrap()
        .prepare("SELECT status_code, key_id, error FROM wb_request_logs ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .flatten()
        .collect()
}

fn chat_body() -> serde_json::Value {
    serde_json::json!({ "model": "deepseek-chat", "messages": [{ "role": "user", "content": "hi" }] })
}

#[tokio::test]
async fn healthz_needs_no_key_and_unknown_paths_get_a_json_404() {
    let (base, conn) = spawn(None).await;
    let client = reqwest::Client::new();

    assert_eq!(client.get(format!("{base}/healthz")).send().await.unwrap().status(), 200);

    let resp = client.get(format!("{base}/v1/nope")).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert!(resp.text().await.unwrap().contains("未知端点"));
    assert!(log_rows(&conn).is_empty(), "routine probe traffic must not fill the log");
}

#[tokio::test]
async fn chat_requires_a_valid_gateway_key() {
    let (base, conn) = spawn(None).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "missing Authorization");

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", "Bearer wk-not-a-real-key")
        .json(&chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unknown key");

    // Both rejections are logged without attributing them to a key.
    let rows = log_rows(&conn);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(code, key_id, _)| *code == 401 && key_id.is_none()));

    // A real key gets past auth and fails later, at the pool.
    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", test_key(&conn)))
        .json(&chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503, "authed, but no upstream protocol configured");
}

#[tokio::test]
async fn authenticated_calls_count_against_the_key_and_the_log() {
    let (base, conn) = spawn(None).await;
    let client = reqwest::Client::new();
    let auth = format!("Bearer {}", test_key(&conn));
    for _ in 0..3 {
        let resp = client
            .post(format!("{base}/v1/chat/completions"))
            .header("authorization", &auth)
            .json(&chat_body())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 503);
    }
    let calls: i64 = conn
        .lock()
        .unwrap()
        .query_row("SELECT call_count FROM wb_api_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(calls, 3);

    let rows = log_rows(&conn);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|(code, key_id, err)| {
        *code == 503 && key_id.is_some() && err.as_deref().is_some_and(|e| e.contains("未配置上游协议"))
    }));
}

#[tokio::test]
async fn a_disabled_key_is_refused_without_touching_the_pool() {
    let (base, conn) = spawn(None).await;
    let (id, key) = conn
        .lock()
        .unwrap()
        .query_row("SELECT id, key FROM wb_api_keys", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    crate::wb_keys::set_enabled(&conn.lock().unwrap(), id, false).unwrap();

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "a revoked key must stop working immediately");
    assert_eq!(log_rows(&conn).len(), 1);
}

#[tokio::test]
async fn bad_request_bodies_are_rejected_before_the_pool_is_consulted() {
    let (base, conn) = spawn(None).await;
    let client = reqwest::Client::new();
    let auth = format!("Bearer {}", test_key(&conn));

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", &auth)
        .body("not json at all")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", &auth)
        .json(&serde_json::json!({ "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "missing model");
    assert!(log_rows(&conn).is_empty(), "a client-side 400 is not an upstream failure");
}

#[tokio::test]
async fn models_is_key_protected_and_falls_back_to_the_configured_list() {
    let profile = serde_json::json!({
        "loginUrl": "https://example.invalid/login",
        "fallbackModels": ["deepseek-chat", "glm-4.5"]
    });
    let (base, conn) = spawn(Some(&profile.to_string())).await;
    let client = reqwest::Client::new();

    assert_eq!(client.get(format!("{base}/v1/models")).send().await.unwrap().status(), 401);

    let resp = client
        .get(format!("{base}/v1/models"))
        .header("authorization", format!("Bearer {}", test_key(&conn)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");
    let ids: Vec<&str> = json["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["deepseek-chat", "glm-4.5"]);
}

#[tokio::test]
async fn an_empty_pool_reports_503_rather_than_hanging() {
    let profile = serde_json::json!({
        "chat": { "method": "POST", "url": "https://example.invalid/chat?model={model}" }
    });
    let (base, conn) = spawn(Some(&profile.to_string())).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {}", test_key(&conn)))
        .json(&chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    assert!(resp.text().await.unwrap().contains("账号池无可用账号"));
}

#[tokio::test]
async fn loopback_is_always_admitted_even_with_a_deny_all_rule() {
    let (base, conn) = spawn(None).await;
    conn.lock().unwrap().execute(
        "INSERT INTO wb_ip_rules (id, kind, ip_or_cidr, enabled, created_at) VALUES ('r1','deny','127.0.0.0/8',1,1)",
        [],
    ).unwrap();
    // The peer is loopback, so the gate exempts it before the rules are read.
    let resp = reqwest::Client::new()
        .get(format!("{base}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------------------------------------------------------------------------
// Stream-only upstream adaptation
// ---------------------------------------------------------------------------

/// A raw TCP upstream that answers every request with the same SSE body.
/// Tokio sockets, no HTTP framework — this only needs to speak one response.
async fn spawn_sse_upstream() -> String {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tauri::async_runtime::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            tauri::async_runtime::spawn(async move {
                let body = concat!(
                    "data: {\"id\":\"c9\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"你\"}}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"}}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                // Drain until EOF so the socket closes with FIN — dropping with
                // the client's request bytes unread makes Windows send a RST
                // that can land mid-response and flake the gateway read.
                let mut sink = [0u8; 4096];
                while matches!(
                    tokio::io::AsyncReadExt::read(&mut sock, &mut sink).await,
                    Ok(n) if n > 0
                ) {}
            });
        }
    });
    format!("http://{addr}/v2/chat/completions")
}

async fn spawn_gateway_with_mock_upstream() -> (String, Arc<Mutex<Connection>>) {
    let upstream = spawn_sse_upstream().await;
    let profile = serde_json::json!({
        "chat": {
            "method": "POST",
            "url": upstream,
            "headers": [["authorization", "Bearer {token}"]]
        }
    });
    let (base, conn) = spawn(Some(&profile.to_string())).await;
    conn.lock().unwrap().execute(
        "INSERT INTO codebuddy_accounts (label, credential_type, credential, status, enabled, created_at)
         VALUES ('A', 'token', 'tok', 'active', 1, 1)",
        [],
    )
    .unwrap();
    (base, conn)
}

#[tokio::test]
async fn a_stream_false_client_gets_one_aggregated_completion() {
    let (base, conn) = spawn_gateway_with_mock_upstream().await;
    let auth = format!("Bearer {}", test_key(&conn));
    let mut body = chat_body();
    body["stream"] = serde_json::Value::Bool(false);

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", &auth)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = resp.text().await.unwrap_or_default();
    assert_eq!(status, 200, "body: {text}");
    assert!(
        ctype.starts_with("application/json"),
        "a non-streaming client must not receive SSE"
    );
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["object"], "chat.completion");
    assert_eq!(json["id"], "c9");
    assert_eq!(json["choices"][0]["message"]["role"], "assistant");
    assert_eq!(json["choices"][0]["message"]["content"], "你好");
    assert_eq!(json["choices"][0]["finish_reason"], "stop");
    assert_eq!(json["usage"]["prompt_tokens"], 7);
    assert_eq!(json["usage"]["completion_tokens"], 2);

    // The call is logged as non-streaming with the usage the frames carried.
    let (stream, prompt, completion, status): (i64, Option<i64>, Option<i64>, i64) = conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT stream, prompt_tokens, completion_tokens, status_code FROM wb_request_logs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((stream, status), (0, 200));
    assert_eq!((prompt, completion), (Some(7), Some(2)));
}

#[tokio::test]
async fn a_stream_true_client_still_gets_verbatim_sse() {
    let (base, conn) = spawn_gateway_with_mock_upstream().await;
    let auth = format!("Bearer {}", test_key(&conn));
    let mut body = chat_body();
    body["stream"] = serde_json::Value::Bool(true);

    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", &auth)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = resp.text().await.unwrap_or_default();
    assert_eq!(status, 200, "body: {text}");
    assert!(
        ctype.contains("text/event-stream"),
        "streaming passthrough must keep the SSE content type"
    );
    assert!(
        text.contains("[DONE]"),
        "frames must pass through untouched: {text}"
    );

    let (stream, prompt, completion): (i64, Option<i64>, Option<i64>) = conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT stream, prompt_tokens, completion_tokens FROM wb_request_logs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stream, 1);
    assert_eq!((prompt, completion), (Some(7), Some(2)));
}

#[test]
fn normalize_models_accepts_the_upstream_shapes_we_might_meet() {
    let fallback = vec!["fb".to_string()];

    let openai = serde_json::json!({ "object": "list", "data": [{ "id": "a" }, { "id": "b" }] });
    let listed = normalize_models(openai, &fallback);
    let ids: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["a", "b"]);

    assert_eq!(normalize_models(serde_json::json!(["x", "y"]), &fallback)["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        normalize_models(serde_json::json!({ "models": [{ "model": "deep" }] }), &fallback)["data"][0]["id"],
        "deep"
    );
    // Nothing usable from upstream -> the configured fallback is what a client sees.
    assert_eq!(normalize_models(serde_json::json!({}), &fallback)["data"][0]["id"], "fb");
    assert_eq!(normalize_models(serde_json::Value::Null, &fallback)["data"][0]["id"], "fb");
    // Every entry keeps the OpenAI envelope.
    let listed = normalize_models(serde_json::json!(["only"]), &fallback);
    assert_eq!(listed["data"][0]["object"], "model");
}
