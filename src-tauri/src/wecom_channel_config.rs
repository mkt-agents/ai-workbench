//! Effective channel configuration, independent of socket/process I/O.
//! Snapshots retain only field digests; callers log field names, never values/digests.
use std::collections::BTreeMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::wecom_channel::{
    effective_mcp, normalize_answer_mode, normalize_engine, WecomApplyRequest,
};
use crate::wecom_dsh::Tier;
use crate::wecom_policy::{DmPolicy, GroupPolicy};

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct ConfigSnapshot {
    fields: BTreeMap<&'static str, [u8; 32]>,
}

pub(crate) struct ConfigDelta {
    pub changed: Vec<&'static str>,
    pub retire: bool,
}

const COLD_FIELDS: &[&str] = &[
    "enabled",
    "botId",
    "botSecret",
    "engine",
    "permission",
    "workspace",
    "mcp",
    "accessPolicy",
    "pairedUsers",
    "agentPolicy",
    "skywalking",
];

impl ConfigSnapshot {
    fn put(&mut self, key: &'static str, value: Value) {
        // Value serialization is infallible; no raw request or credential is retained.
        self.fields
            .insert(key, Sha256::digest(value.to_string().as_bytes()).into());
    }

    pub fn set_paired_users(&mut self, users: &[String]) {
        self.put("pairedUsers", json!(normalized_set(users)));
    }

    pub fn delta(&self, previous: Option<&Self>) -> ConfigDelta {
        let changed: Vec<_> = self
            .fields
            .keys()
            .chain(previous.into_iter().flat_map(|p| p.fields.keys()))
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|key| previous.and_then(|p| p.fields.get(key)) != self.fields.get(key))
            .collect();
        let retire = previous.is_none() || changed.iter().any(|key| COLD_FIELDS.contains(key));
        ConfigDelta { changed, retire }
    }
}

fn normalized_set(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn default_text(value: &str, default: &str) -> String {
    if value.trim().is_empty() {
        default.into()
    } else {
        value.trim().into()
    }
}

/// Decryption is injected so fixtures exercise random-ciphertext equivalence without DPAPI.
/// Only active-engine secrets are decoded. Disabling never depends on valid bot credentials.
pub(crate) fn normalize_request(
    mut req: WecomApplyRequest,
    decrypt: impl Fn(&str) -> Result<String, String>,
) -> Result<WecomApplyRequest, String> {
    req.engine = normalize_engine(&req.engine);
    req.bot_id = req.bot_id.trim().into();
    req.bot_secret = if req.enabled {
        decrypt(req.bot_secret.trim()).map_err(|_| "Secret 解密失败".to_string())?
    } else {
        String::new()
    };
    req.corp_id = req.corp_id.trim().into();
    req.corp_secret = if req.enabled && !req.corp_secret.trim().is_empty() {
        decrypt(req.corp_secret.trim()).map_err(|_| "应用 Secret 解密失败".to_string())?
    } else {
        String::new()
    };
    req.webhook_url = if req.webhook_url.trim().is_empty() {
        String::new()
    } else {
        decrypt(req.webhook_url.trim()).map_err(|_| "Webhook 解密失败".to_string())?
    };
    req.bot_display_name = req.bot_display_name.trim().into();
    // UI and backend share a Unicode-character budget, not a UTF-8 byte budget.
    req.self_intro = req.self_intro.trim().chars().take(2000).collect();
    req.workspace = if req.workspace.trim().is_empty() {
        req.agent
            .as_ref()
            .map(|a| a.repo_path.trim().to_owned())
            .unwrap_or_default()
    } else {
        req.workspace.trim().into()
    };
    let tier = if req.permission.trim().is_empty() {
        req.agent
            .as_ref()
            .map(|a| a.permission.as_str())
            .unwrap_or("")
    } else {
        &req.permission
    };
    req.permission = Tier::parse(tier).label().into();
    req.mcp = Some(effective_mcp(
        req.mcp.take(),
        req.mcp_enabled,
        req.agent.as_ref(),
    ));
    req.mcp_enabled = Some(true); // resolved list, including explicit disabled -> empty
    req.allow_users = normalized_set(&req.allow_users);
    req.allow_groups = normalized_set(&req.allow_groups);
    req.paired_users = normalized_set(&req.paired_users);
    req.dsh_model = req.dsh_model.trim().into();
    req.dsh_effort = req.dsh_effort.trim().into();
    req.answer_mode = normalize_answer_mode(&req.answer_mode);
    req.doc_dir = default_text(&req.doc_dir, "doc");
    req.auto_update_time = default_text(&req.auto_update_time, "23:00");
    req.release_branch = default_text(&req.release_branch, "release");
    req.skywalking_addr = req.skywalking_addr.trim().into();
    req.skywalking_header = req.skywalking_header.trim().into();
    req.max_jobs = if (1..=crate::wecom_admission::MAX_LIMIT).contains(&req.max_jobs) {
        req.max_jobs
    } else {
        crate::wecom_admission::DEFAULT_LIMIT
    };
    if req.enabled && req.engine == "tianji" {
        if let Some(agent) = req.agent.as_mut() {
            agent.config.api_key =
                decrypt(agent.config.api_key.trim()).map_err(|_| "模型密钥解密失败".to_string())?;
            if let Some(fallback) = agent.fallback_config.as_mut() {
                fallback.api_key = decrypt(fallback.api_key.trim())
                    .map_err(|_| "备用模型密钥解密失败".to_string())?;
            }
            agent.web_search_key =
                decrypt(agent.web_search_key.trim()).map_err(|_| "搜索密钥解密失败".to_string())?;
        }
    }
    Ok(req)
}

fn model_value(model: &crate::ai_commands::AIModelConfig) -> Value {
    // DB row identity, display labels and random encryption are not model routing.
    json!({"provider": model.provider, "apiKey": model.api_key,
        "authType": model.auth_type, "baseUrl": model.base_url, "model": model.model,
        "temperature": model.temperature, "maxTokens": model.max_tokens, "vision": model.vision})
}

pub(crate) fn snapshot(req: &WecomApplyRequest) -> ConfigSnapshot {
    let mut out = ConfigSnapshot::default();
    out.put("enabled", json!(req.enabled));
    out.put("botId", json!(req.bot_id));
    out.put("botSecret", json!(req.bot_secret));
    out.put("engine", json!(req.engine));
    out.put(
        "workspace",
        json!([
            req.workspace,
            if req.engine == "tianji" {
                req.agent.as_ref().map(|a| a.repo_path.trim()).unwrap_or("")
            } else {
                ""
            }
        ]),
    );
    out.put(
        "permission",
        json!(if req.engine == "dsh" {
            req.permission.as_str()
        } else {
            match req
                .agent
                .as_ref()
                .map(|a| a.permission.trim())
                .unwrap_or("")
            {
                "workspace" => "workspace",
                "review" => "review",
                "full" => "full",
                _ => "read",
            }
        }),
    );
    out.put(
        "accessPolicy",
        json!({"dm": DmPolicy::parse(&req.dm_policy),
        "group": GroupPolicy::parse(&req.group_policy), "mention": req.require_mention,
        "users": req.allow_users, "groups": req.allow_groups}),
    );
    out.set_paired_users(&req.paired_users);
    let mcp = if req.engine == "dsh" {
        req.mcp.as_deref().unwrap_or_default()
    } else {
        req.agent
            .as_ref()
            .filter(|a| a.mcp_enabled)
            .map(|a| a.mcp.as_slice())
            .unwrap_or_default()
    };
    out.put(
        "mcp",
        Value::Array(
            mcp.iter()
                .filter(|s| !s.name.trim().is_empty() && !s.command.trim().is_empty())
                .map(|s| {
                    json!({"name": s.name.trim(), "command": s.command.trim(),
            "args": crate::mcp::split_args(&s.args)})
                })
                .collect(),
        ),
    );
    out.put(
        "skywalking",
        json!([
            req.skywalking_addr,
            req.skywalking_header,
            req.skywalking_timezone_offset_minutes
        ]),
    );
    out.put("corpCredentials", json!([req.corp_id, req.corp_secret]));
    out.put("botDisplayName", json!(req.bot_display_name));
    out.put("selfIntro", json!(req.self_intro));
    out.put("maxJobs", json!(req.max_jobs));
    out.put("answerMode", json!(req.answer_mode));
    out.put(
        "docgen",
        json!({"enabled":req.auto_update_enabled, "time":req.auto_update_time,
        "branch":req.release_branch, "docDir":req.doc_dir, "webhook":req.webhook_url,
        "autoCommit":req.auto_commit, "autoPush":req.auto_push}),
    );
    if req.engine == "dsh" {
        out.put("model", json!(req.dsh_model));
        out.put("effort", json!(req.dsh_effort));
    } else if let Some(a) = &req.agent {
        out.put(
            "model",
            json!([
                model_value(&a.config),
                a.fallback_config.as_ref().map(model_value)
            ]),
        );
        out.put(
            "agentPolicy",
            json!({"services": normalized_set(&a.services), "tools":a.tools,
            "mode":a.mode, "vision":a.vision, "diffConfirm":a.diff_confirm,
            "alwaysAllow":normalized_set(&a.always_allow), "webAllow":normalized_set(&a.web_allow),
            "webTier":a.web_tier, "webSearchProvider":a.web_search_provider,
            "webSearchKey":a.web_search_key, "subagents":a.subagents}),
        );
        out.put("agentTuning", json!({"parallelReads":a.parallel_reads,"retries":a.retries,
            "grepIgnore":a.grep_ignore,"planFirst":a.plan_first,"contextTokens":a.context_tokens,
            "resultCap":a.result_cap,"anthropicStream":a.anthropic_stream,"systemExtra":a.system_extra}));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WecomApplyRequest {
        serde_json::from_value(json!({
            "enabled":true,"botId":"fixture-bot","botSecret":"fixture-secret",
            "engine":"dsh","permission":"read","workspace":"D:\\fixture",
            "mcp":[],"mcpEnabled":true,
            "agent":{"config":{"name":"fixture","provider":"fixture","apiKey":"fixture-key",
                "baseUrl":"https://fixture.invalid","model":"fixture-model","temperature":0.0,
                "maxTokens":100,"isDefault":false}}
        }))
        .unwrap()
    }

    fn plain(req: WecomApplyRequest) -> ConfigSnapshot {
        snapshot(&normalize_request(req, |s| Ok(s.into())).unwrap())
    }

    #[test]
    fn randomized_ciphertexts_with_same_plaintext_are_noop() {
        let decode = |s: &str| -> Result<String, String> {
            match s {
                "enc:v1:first" | "enc:v1:second" => Ok("fixture-plaintext".into()),
                _ => Ok(s.into()),
            }
        };
        let mut a = request();
        let mut b = request();
        a.bot_secret = "enc:v1:first".into();
        b.bot_secret = "enc:v1:second".into();
        a.corp_secret = a.bot_secret.clone();
        b.corp_secret = b.bot_secret.clone();
        a.webhook_url = a.bot_secret.clone();
        b.webhook_url = b.bot_secret.clone();
        // Reproduce the previous whole-request fingerprint trigger with only stored bytes changed.
        assert_ne!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
        let a = snapshot(&normalize_request(a, decode).unwrap());
        let b = snapshot(&normalize_request(b, decode).unwrap());
        let delta = b.delta(Some(&a));
        assert!(delta.changed.is_empty());
        assert!(!delta.retire);
    }

    #[test]
    fn self_intro_hot_update_truncates_without_retiring() {
        let a = plain(request());
        let mut b = request();
        b.self_intro = "你好！我是腾采通智能助手-超级玛丽 👋".into();
        let b = plain(b);
        let delta = b.delta(Some(&a));
        assert_eq!(delta.changed, vec!["selfIntro"]);
        assert!(!delta.retire);
        let mut long = request();
        long.self_intro = "字".repeat(900); // 2700 UTF-8 bytes must not truncate a 900-character intro.
        let normalized = normalize_request(long, |s| Ok(s.into())).unwrap();
        assert_eq!(normalized.self_intro.chars().count(), 900);
        let mut over = request();
        over.self_intro = "👋".repeat(2001);
        let normalized = normalize_request(over, |s| Ok(s.into())).unwrap();
        assert_eq!(normalized.self_intro.chars().count(), 2000);
    }

    #[test]
    fn dsh_ignores_unrelated_tianji_model_including_its_ciphertext() {
        let a = plain(request());
        let mut b = request();
        let model = &mut b.agent.as_mut().unwrap().config;
        model.model = "other-model".into();
        model.api_key = "enc:v1:not-read-for-dsh".into();
        model.updated_at = Some("new timestamp".into());
        let b = normalize_request(b, |s| {
            assert!(!s.starts_with("enc:"));
            Ok(s.into())
        })
        .unwrap();
        assert!(snapshot(&b).delta(Some(&a)).changed.is_empty());
    }

    #[test]
    fn model_effort_jobs_answer_and_docgen_are_hot() {
        let a = plain(request());
        let mut b = request();
        b.dsh_model = "[\"fixture\",\"new\"]".into();
        b.dsh_effort = "high".into();
        b.max_jobs = 32;
        b.answer_mode = "code".into();
        b.auto_update_enabled = true;
        b.webhook_url = "fixture-hook".into();
        let delta = plain(b).delta(Some(&a));
        assert_eq!(
            delta.changed,
            vec!["answerMode", "docgen", "effort", "maxJobs", "model"]
        );
        assert!(!delta.retire);
    }

    #[test]
    fn mcp_access_credentials_permission_workspace_and_engine_retire() {
        let a = plain(request());
        for field in [
            "mcp",
            "accessPolicy",
            "botSecret",
            "botId",
            "permission",
            "workspace",
            "engine",
            "enabled",
        ] {
            let mut b = request();
            match field {
                "mcp" => {
                    b.mcp = Some(vec![crate::mcp::McpServerSpec {
                        name: "fixture".into(),
                        command: "fixture-command".into(),
                        args: "--fixture".into(),
                    }])
                }
                "accessPolicy" => b.allow_users.push("fixture-user".into()),
                "botSecret" => b.bot_secret = "new-secret".into(),
                "botId" => b.bot_id = "other-bot".into(),
                "permission" => b.permission = "full".into(),
                "workspace" => b.workspace = "D:\\other".into(),
                "engine" => b.engine = "tianji".into(),
                "enabled" => b.enabled = false,
                _ => unreachable!(),
            }
            let delta = plain(b).delta(Some(&a));
            assert!(delta.retire, "{field}");
            assert!(delta.changed.contains(&field), "{field}");
        }
    }

    #[test]
    fn normalized_defaults_sets_and_inactive_mcp_are_noop() {
        let mut a = request();
        a.allow_users = vec!["b".into(), "a".into()];
        a.mcp_enabled = Some(false);
        let a = plain(a);
        let mut b = request();
        b.engine = " DSH ".into();
        b.permission = " READ ".into();
        b.doc_dir = "doc".into();
        b.auto_update_time = "23:00".into();
        b.release_branch = "release".into();
        b.answer_mode = "AUTO".into();
        b.max_jobs = 2;
        b.allow_users = vec![" a ".into(), "b".into(), "a".into(), "".into()];
        b.mcp_enabled = Some(false);
        b.mcp = Some(vec![crate::mcp::McpServerSpec {
            name: "ignored".into(),
            command: "ignored".into(),
            args: "".into(),
        }]);
        assert!(plain(b).delta(Some(&a)).changed.is_empty());
    }

    #[test]
    fn mcp_arguments_follow_runtime_tokenization() {
        let mut a = request();
        a.mcp = Some(vec![crate::mcp::McpServerSpec {
            name: " fixture ".into(),
            command: " fixture-command ".into(),
            args: "--key   value".into(),
        }]);
        let a = plain(a);
        let mut b = request();
        b.mcp = Some(vec![crate::mcp::McpServerSpec {
            name: "fixture".into(),
            command: "fixture-command".into(),
            args: "--key value".into(),
        }]);
        assert!(plain(b).delta(Some(&a)).changed.is_empty());
    }

    #[test]
    fn tianji_model_secret_equivalence_and_actual_policy() {
        let mut a = request();
        a.engine = "tianji".into();
        a.agent.as_mut().unwrap().config.api_key = "enc:v1:first".into();
        let mut b = request();
        b.engine = "tianji".into();
        b.permission = "full".into(); // Tianji uses agent.permission, not this DSH copy.
        b.agent.as_mut().unwrap().config.api_key = "enc:v1:second".into();
        let decode = |s: &str| {
            Ok(if s.starts_with("enc:") {
                "plain".into()
            } else {
                s.into()
            })
        };
        let a = snapshot(&normalize_request(a, decode).unwrap());
        let b = snapshot(&normalize_request(b, decode).unwrap());
        assert!(b.delta(Some(&a)).changed.is_empty());
    }

    #[test]
    fn dsh_legacy_permission_workspace_and_mcp_remain_effective() {
        let mut a = request();
        a.permission.clear();
        a.workspace.clear();
        a.mcp = None;
        a.mcp_enabled = None;
        let agent = a.agent.as_mut().unwrap();
        agent.permission = "read".into();
        agent.repo_path = "D:\\fixture".into();
        let mut b: WecomApplyRequest =
            serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
        b.agent.as_mut().unwrap().permission = "full".into();
        let delta = plain(b).delta(Some(&plain(a)));
        assert!(delta.retire);
        assert_eq!(delta.changed, vec!["permission"]);
    }

    #[test]
    fn mirrored_paired_users_are_noop_but_new_access_retires() {
        let mut old = plain(request());
        old.set_paired_users(&["live-user".into()]);
        let mut mirrored = plain(request());
        mirrored.set_paired_users(&["live-user".into(), "live-user".into()]);
        assert!(mirrored.delta(Some(&old)).changed.is_empty());
        mirrored.set_paired_users(&["live-user".into(), "new-user".into()]);
        let delta = mirrored.delta(Some(&old));
        assert!(delta.retire);
        assert_eq!(delta.changed, vec!["pairedUsers"]);
    }

    #[test]
    fn secret_decode_errors_are_value_free() {
        let error =
            normalize_request(request(), |_| Err("raw secret material".into())).unwrap_err();
        assert_eq!(error, "Secret 解密失败");
    }

    #[test]
    fn empty_effort_is_hot_but_does_not_claim_provider_reset() {
        let mut a = request();
        a.dsh_effort = "high".into();
        let a = plain(a);
        let delta = plain(request()).delta(Some(&a));
        assert_eq!(delta.changed, vec!["effort"]);
        assert!(!delta.retire);
        // configure clears the local selection cache; ACP currently sends no
        // reasoning_effort RPC for empty values, so existing sessions may retain high.
    }
}
