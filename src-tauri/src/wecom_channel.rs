//! 企微智能机器人通道：生命周期、Tauri 命令、与设置页对接。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use crate::wecom_orchestrator::{Orchestrator, WecomAgentConfig};
use crate::wecom_policy::{DmPolicy, GroupPolicy, PendingPair, PolicyState};
use crate::wecom_ws::WecomSocket;

pub struct WecomState {
    pub sock: Arc<WecomSocket>,
    pub orch: Arc<Orchestrator>,
    /// 上次成功 apply 的 bot 凭据指纹，避免无变化反复踢线
    pub applied: Mutex<Option<AppliedCreds>>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AppliedCreds {
    bot_id: String,
    secret: String,
}

impl WecomState {
    pub fn new() -> Self {
        let sock = Arc::new(WecomSocket::new());
        let orch = Arc::new(Orchestrator::new(Arc::clone(&sock)));
        Self {
            sock,
            orch,
            applied: Mutex::new(None),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomApplyRequest {
    pub enabled: bool,
    pub bot_id: String,
    /// 明文 Secret（前端在保存时经 protect 存档；apply 时传解密后的明文或仍带 enc:v1:）
    pub bot_secret: String,
    #[serde(default)]
    pub bot_display_name: String,
    #[serde(default)]
    pub dm_policy: String,
    #[serde(default)]
    pub group_policy: String,
    #[serde(default = "default_true")]
    pub require_mention: bool,
    #[serde(default)]
    pub allow_users: Vec<String>,
    #[serde(default)]
    pub allow_groups: Vec<String>,
    #[serde(default)]
    pub paired_users: Vec<String>,
    /// 绑定天玑跑回合所需的模型与权限；enabled 时必填
    #[serde(default)]
    pub agent: Option<WecomAgentConfig>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomStatus {
    pub state: String,
    pub detail: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomPairingView {
    pub userid: String,
    pub code: String,
    pub created_ms: u64,
}

#[tauri::command]
pub async fn wecom_channel_status(state: State<'_, WecomState>) -> Result<WecomStatus, String> {
    let (s, detail) = state.sock.status().await;
    let enabled = state.applied.lock().await.is_some();
    Ok(WecomStatus {
        state: s,
        detail,
        enabled,
    })
}

#[tauri::command]
pub async fn wecom_channel_apply(
    app: AppHandle,
    state: State<'_, WecomState>,
    req: WecomApplyRequest,
) -> Result<WecomStatus, String> {
    // 解密 Secret
    let secret = crate::secret_protect::unprotect_secret(req.bot_secret.trim())
        .map_err(|e| format!("Secret 解密失败: {e}"))?;

    // 更新策略与 agent 配置
    {
        let mut policy = state.orch.policy.lock().await;
        policy.dm_policy = DmPolicy::parse(&req.dm_policy);
        policy.group_policy = GroupPolicy::parse(&req.group_policy);
        policy.require_mention = req.require_mention;
        policy.allow_users = req
            .allow_users
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        policy.allow_groups = req
            .allow_groups
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // 合并前端持久化的已配对用户（不丢内存里尚未同步的）
        for u in req.paired_users {
            let u = u.trim().to_string();
            if !u.is_empty() && !policy.paired_users.iter().any(|x| x == &u) {
                policy.paired_users.push(u);
            }
        }
    }
    {
        *state.orch.bot_display_name.lock().await = req.bot_display_name.trim().to_string();
    }
    if let Some(agent) = req.agent {
        *state.orch.agent.lock().await = Some(agent);
    }

    if !req.enabled {
        state.sock.stop().await;
        *state.applied.lock().await = None;
        let st = wecom_channel_status(state).await?;
        emit_status(&app, &st);
        return Ok(st);
    }

    let bot_id = req.bot_id.trim().to_string();
    if bot_id.is_empty() || secret.is_empty() {
        return Err("请填写 Bot ID 与 Secret".into());
    }
    if state.orch.agent.lock().await.is_none() {
        return Err("请先配置天玑模型，再启用企微通道".into());
    }

    let creds = AppliedCreds {
        bot_id: bot_id.clone(),
        secret: secret.clone(),
    };
    let same = state.applied.lock().await.as_ref() == Some(&creds);
    if !same {
        // 换凭据：断旧连再开
        state.sock.stop().await;
        *state.applied.lock().await = Some(creds);

        let orch = Arc::clone(&state.orch);
        let app_msg = app.clone();
        let app_ev = app.clone();
        let on_msg = Arc::new(move |msg| {
            orch.handle_msg(app_msg.clone(), msg);
        });
        let orch2 = Arc::clone(&state.orch);
        let on_event = Arc::new(move |ev| {
            orch2.handle_event(app_ev.clone(), ev);
        });
        state
            .sock
            .start(app.clone(), bot_id, secret, on_msg, on_event);
    }

    // 等一小会儿让 Connecting → Connected/Error 有机会更新
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let st = wecom_channel_status(state).await?;
    emit_status(&app, &st);
    Ok(st)
}

#[tauri::command]
pub async fn wecom_channel_restart(
    app: AppHandle,
    state: State<'_, WecomState>,
) -> Result<WecomStatus, String> {
    let creds = state
        .applied
        .lock()
        .await
        .clone()
        .ok_or_else(|| "通道未启用".to_string())?;
    state.sock.stop().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let orch = Arc::clone(&state.orch);
    let app_msg = app.clone();
    let app_ev = app.clone();
    let on_msg = Arc::new(move |msg| {
        orch.handle_msg(app_msg.clone(), msg);
    });
    let orch2 = Arc::clone(&state.orch);
    let on_event = Arc::new(move |ev| {
        orch2.handle_event(app_ev.clone(), ev);
    });
    state.sock.start(
        app.clone(),
        creds.bot_id.clone(),
        creds.secret.clone(),
        on_msg,
        on_event,
    );
    *state.applied.lock().await = Some(creds);

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let st = wecom_channel_status(state).await?;
    emit_status(&app, &st);
    Ok(st)
}
#[tauri::command]
pub async fn wecom_pairing_list(
    state: State<'_, WecomState>,
) -> Result<Vec<WecomPairingView>, String> {
    let mut policy = state.orch.policy.lock().await;
    policy.prune_pending();
    Ok(policy
        .pending
        .iter()
        .map(|p| WecomPairingView {
            userid: p.userid.clone(),
            code: p.code.clone(),
            created_ms: p.created_ms,
        })
        .collect())
}

#[tauri::command]
pub async fn wecom_pairing_approve(
    state: State<'_, WecomState>,
    code: String,
) -> Result<String, String> {
    let mut policy = state.orch.policy.lock().await;
    policy.approve_pair(&code)
}

#[tauri::command]
pub async fn wecom_pairing_revoke(
    state: State<'_, WecomState>,
    userid: String,
) -> Result<(), String> {
    let mut policy = state.orch.policy.lock().await;
    policy.revoke_user(userid.trim());
    Ok(())
}

#[tauri::command]
pub async fn wecom_paired_users(state: State<'_, WecomState>) -> Result<Vec<String>, String> {
    let policy = state.orch.policy.lock().await;
    Ok(policy.paired_users.clone())
}

#[tauri::command]
pub fn wecom_protect_secret(plain: String) -> Result<String, String> {
    crate::secret_protect::protect_secret(&plain)
}

#[tauri::command]
pub fn wecom_reveal_secret(stored: String) -> Result<String, String> {
    crate::secret_protect::unprotect_secret(stored.trim())
}

fn emit_status(app: &AppHandle, st: &WecomStatus) {
    let _ = app.emit("wecom-channel-status", st);
}

/// setup 时挂上 State；真正连线等前端 apply。
pub fn init_state() -> WecomState {
    WecomState::new()
}

// silence unused import in some builds
#[allow(dead_code)]
fn _touch_policy(_: &PolicyState, _: &PendingPair) {}
