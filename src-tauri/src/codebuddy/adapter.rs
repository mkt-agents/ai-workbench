//! Protocol convergence layer: every CodeBuddy endpoint the WorkBuddy domain
//! talks to is described here as data (JSON in `wb_settings.adapter_json`),
//! never as code. Pinning the real protocol down after a packet capture is a
//! settings edit, not a rebuild.

use serde::{Deserialize, Serialize};

/// One upstream endpoint. `{placeholders}` in `url`, `headers` values and
/// `body` are rendered from the request context (credential fields, model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// HTTP method, e.g. "GET" / "POST".
    pub method: String,
    pub url: String,
    /// Header name -> value template.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Request body template; ignored for GET.
    #[serde(default)]
    pub body: Option<String>,
}

/// The full configurable picture of one CodeBuddy deployment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProfile {
    /// Login / QR page opened in the embedded webview.
    #[serde(default)]
    pub login_url: String,
    /// Verifies a credential is still alive (e.g. a /user/info call).
    #[serde(default)]
    pub credential_probe: Option<Endpoint>,
    /// Daily check-in for points.
    #[serde(default)]
    pub checkin: Option<Endpoint>,
    /// Queries today's check-in status (streak / credits) without claiming.
    #[serde(default)]
    pub checkin_status: Option<Endpoint>,
    /// Growth-plan streak ladder (read-only display).
    #[serde(default)]
    pub growth_streak: Option<Endpoint>,
    /// Growth-plan daily heatmap (fetched but not yet rendered).
    #[serde(default)]
    pub growth_heatmap: Option<Endpoint>,
    /// Growth-plan task list (read-only display).
    #[serde(default)]
    pub growth_tasks: Option<Endpoint>,
    /// The actual chat upstream the gateway proxies to. `{model}` is available.
    #[serde(default)]
    pub chat: Option<Endpoint>,
    /// Model listing; when absent the gateway serves a static fallback.
    #[serde(default)]
    pub models: Option<Endpoint>,
    /// Models advertised by /v1/models when there is no upstream listing.
    #[serde(default)]
    pub fallback_models: Vec<String>,
}

/// A fully rendered request, ready to be handed to reqwest. Kept free of any
/// HTTP dependency so template rendering is unit-testable.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Context substituted into endpoint templates.
pub struct RenderContext<'a> {
    pub credential: &'a str,
    pub token: &'a str,
    pub cookie: &'a str,
    pub model: &'a str,
}

fn render(template: &str, cx: &RenderContext) -> String {
    template
        .replace("{credential}", cx.credential)
        .replace("{token}", cx.token)
        .replace("{cookie}", cx.cookie)
        .replace("{model}", cx.model)
}

/// Build the concrete request for `ep`. `None` only when `method` is unusable
/// (guarded so a half-filled settings row cannot panic the gateway).
pub fn build_request(ep: &Endpoint, cx: &RenderContext) -> Option<OutboundRequest> {
    let method = ep.method.trim().to_uppercase();
    if method.is_empty() {
        return None;
    }
    let body = if method == "GET" {
        None
    } else {
        ep.body.as_deref().map(|b| render(b, cx))
    };
    Some(OutboundRequest {
        method,
        url: render(&ep.url, cx),
        headers: ep
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), render(v, cx)))
            .collect(),
        body,
    })
}

impl UpstreamProfile {
    /// Parse the adapter_json blob. Errors are returned as strings so the UI
    /// can show *why* the stored protocol is broken instead of silently
    /// falling back.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("协议配置解析失败: {e}"))
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx<'a>(credential: &'a str, token: &'a str, cookie: &'a str) -> RenderContext<'a> {
        RenderContext { credential, token, cookie, model: "deepseek-chat" }
    }

    fn ep(url: &str) -> Endpoint {
        Endpoint { method: "post".into(), url: url.into(), headers: vec![], body: None }
    }

    #[test]
    fn renders_all_placeholders_in_url_headers_and_body() {
        let endpoint = Endpoint {
            method: "post".into(),
            url: "https://x/api/chat?m={model}&c={credential}".into(),
            headers: vec![
                ("authorization".into(), "Bearer {token}".into()),
                ("cookie".into(), "{cookie}".into()),
            ],
            body: Some(r#"{"prompt":"hi {credential}"}"#.into()),
        };
        let req = build_request(&endpoint, &cx("CRED", "TK", "CK")).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://x/api/chat?m=deepseek-chat&c=CRED");
        assert_eq!(req.headers[0], ("authorization".into(), "Bearer TK".into()));
        assert_eq!(req.headers[1], ("cookie".into(), "CK".into()));
        assert_eq!(req.body.unwrap(), r#"{"prompt":"hi CRED"}"#);
    }

    #[test]
    fn get_requests_never_carry_a_body() {
        let endpoint = Endpoint {
            method: "get".into(),
            url: "https://x/user".into(),
            headers: vec![],
            body: Some("{}".into()),
        };
        let req = build_request(&endpoint, &cx("C", "T", "K")).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.body, None);
    }

    #[test]
    fn empty_method_is_rejected_not_panicking() {
        let endpoint = ep("https://x");
        let bad = Endpoint { method: "   ".into(), ..endpoint };
        assert!(build_request(&bad, &cx("C", "T", "K")).is_none());
    }

    #[test]
    fn the_shipped_template_is_a_profile_the_backend_actually_accepts() {
        // 模板是给用户一键粘进「协议配置」的，它解析不了就等于功能不可用；而端点是否
        // 真实存在只有打一次才知道。copilot.tencent.com/api/* 实测 404，别再写回去。
        let json = r#"{
          "loginUrl": "https://www.codebuddy.cn/login/?platform=usercenter",
          "credentialProbe": { "method": "GET", "url": "https://api.lkeap.cloud.tencent.com/plan/v3/models",
                               "headers": [["authorization", "Bearer {token}"]] },
          "checkin": null,
          "chat": { "method": "POST", "url": "https://api.lkeap.cloud.tencent.com/plan/v3/chat/completions",
                    "headers": [["authorization", "Bearer {token}"], ["content-type", "application/json"]] },
          "models": null,
          "fallbackModels": ["deepseek-chat"]
        }"#;
        let profile = UpstreamProfile::from_json(json).unwrap();
        let probe = profile.credential_probe.unwrap();
        assert_eq!(probe.method.to_uppercase(), "GET");
        assert!(probe.url.contains("/plan/v3/models"), "{}", probe.url);
        assert!(profile.checkin.is_none());
        assert!(profile.chat.unwrap().url.contains("/plan/v3/chat/completions"));
        assert!(!json.contains("copilot.tencent.com/api/"));
        // 端点没把握就整段删掉：可选字段必须能缺省，否则编辑器逼用户填猜测值。
        let sparse = UpstreamProfile::from_json(r#"{ "loginUrl": "https://x/login" }"#).unwrap();
        assert!(sparse.credential_probe.is_none() && sparse.chat.is_none());
    }

    #[test]
    fn profile_round_trips_through_json_with_optional_endpoints() {
        let profile = UpstreamProfile {
            login_url: "https://x/login".into(),
            credential_probe: Some(ep("https://x/user")),
            checkin: None,
            chat: Some(ep("https://x/chat")),
            models: None,
            fallback_models: vec!["deepseek-chat".into()],
            ..Default::default()
        };
        let json = profile.to_json().unwrap();
        let back = UpstreamProfile::from_json(&json).unwrap();
        assert_eq!(profile, back);
    }

    #[test]
    fn broken_json_surfaces_reason() {
        let err = UpstreamProfile::from_json("{ not json").unwrap_err();
        assert!(err.contains("协议配置解析失败"));
    }
}
