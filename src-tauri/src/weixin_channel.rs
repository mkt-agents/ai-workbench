//! 个人微信 iLink 通道：apply / status / restart；DPAPI 存 token；游标落 APPDATA。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

use crate::wecom_orchestrator::WecomAgentConfig;
use crate::weixin_orchestrator::{WeixinOrchestrator, WeixinRuntime};

pub struct WeixinState {
    pub orch: Arc<WeixinOrchestrator>,
    pub applied: Mutex<Option<AppliedWeixin>>,
    pub apply_lock: Mutex<()>,
    /// Reused answer settings from WeCom (engine mode / doc dir live here for status).
    pub answer_mode: Mutex<String>,
    pub doc_dir: Mutex<String>,
    pub data_dir: Mutex<Option<PathBuf>>,
    pub last_agent: Mutex<Option<WecomAgentConfig>>,
}

#[derive(Clone)]
pub struct AppliedWeixin {
    pub token: String,
    pub bot_id: String,
    pub user_id: String,
    pub base_url: String,
}

impl WeixinState {
    pub fn new() -> Self {
        Self {
            orch: Arc::new(WeixinOrchestrator::new()),
            applied: Mutex::new(None),
            apply_lock: Mutex::new(()),
            answer_mode: Mutex::new("auto".into()),
            doc_dir: Mutex::new("doc".into()),
            data_dir: Mutex::new(None),
            last_agent: Mutex::new(None),
        }
    }
}

pub fn init_state() -> WeixinState {
    WeixinState::new()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeixinApplyRequest {
    pub enabled: bool,
    /// DPAPI enc:v1:… or plaintext token
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub bot_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub base_url: String,
    /// Reuse WeCom answer settings
    #[serde(default)]
    pub answer_mode: String,
    #[serde(default)]
    pub doc_dir: String,
    #[serde(default)]
    pub agent: Option<WecomAgentConfig>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeixinStatus {
    pub state: String,
    pub detail: String,
    pub enabled: bool,
    pub bot_id: String,
    pub user_id: String,
    pub base_url: String,
    pub last_error: String,
    pub running: bool,
}

fn cursor_path(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    dir.join("weixin-ilink-cursor.txt")
}

async fn stop_channel(state: &WeixinState) {
    state.orch.request_stop();
    // Give the poll loop a moment to exit.
    for _ in 0..20 {
        if !state.orch.running.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    *state.applied.lock().await = None;
}

async fn start_channel(app: &AppHandle, state: &WeixinState, creds: AppliedWeixin, agent: Option<WecomAgentConfig>) {
    stop_channel(state).await;
    // Fresh orch stop flags
    state.orch.stop.store(false, std::sync::atomic::Ordering::SeqCst);
    let answer_mode = state.answer_mode.lock().await.clone();
    let doc_dir = state.doc_dir.lock().await.clone();
    let rt = WeixinRuntime {
        token: creds.token.clone(),
        base_url: creds.base_url.clone(),
        bot_id: creds.bot_id.clone(),
        bound_user_id: creds.user_id.clone(),
        answer_mode,
        doc_dir,
        agent,
    };
    let path = cursor_path(app);
    *state.data_dir.lock().await = path.parent().map(|p| p.to_path_buf());
    *state.applied.lock().await = Some(creds);
    state.orch.spawn_loop(app.clone(), rt, path);
}

#[tauri::command]
pub async fn weixin_channel_status(state: State<'_, WeixinState>) -> Result<WeixinStatus, String> {
    let applied = state.applied.lock().await.clone();
    let running = state.orch.running.load(std::sync::atomic::Ordering::SeqCst);
    let last_error = state.orch.last_error.lock().await.clone();
    let enabled = applied.is_some();
    let (bot_id, user_id, base_url) = match &applied {
        Some(a) => (a.bot_id.clone(), a.user_id.clone(), a.base_url.clone()),
        None => (String::new(), String::new(), String::new()),
    };
    let state_label = if !enabled {
        "off"
    } else if running && last_error.is_empty() {
        "connected"
    } else if running {
        "degraded"
    } else {
        "stopped"
    };
    let detail = if !enabled {
        "未启用".into()
    } else if running {
        if last_error.is_empty() {
            "长轮询中".into()
        } else {
            last_error.clone()
        }
    } else {
        "已停止".into()
    };
    Ok(WeixinStatus {
        state: state_label.into(),
        detail,
        enabled,
        bot_id,
        user_id,
        base_url,
        last_error,
        running,
    })
}

#[tauri::command]
pub async fn weixin_channel_apply(
    app: AppHandle,
    state: State<'_, WeixinState>,
    req: WeixinApplyRequest,
) -> Result<WeixinStatus, String> {
    {
        let _lock = state.apply_lock.lock().await;
        *state.answer_mode.lock().await = crate::wecom_channel::normalize_answer_mode(&req.answer_mode);
        *state.doc_dir.lock().await = if req.doc_dir.trim().is_empty() {
            "doc".into()
        } else {
            req.doc_dir.trim().into()
        };

        if !req.enabled {
            stop_channel(&state).await;
        } else {
            let token_raw = req.bot_token.trim();
            if token_raw.is_empty() {
                return Err("缺少微信 Bot Token（请先扫码绑定）".into());
            }
            let token = crate::secret_protect::unprotect_secret(token_raw)
                .map_err(|_| "微信 Token 解密失败".to_string())?;
            if token.trim().is_empty() {
                return Err("微信 Bot Token 为空".into());
            }
            let base_url = crate::weixin_ilink::normalize_base_url(&req.base_url)?;
            let creds = AppliedWeixin {
                token: token.trim().to_string(),
                bot_id: req.bot_id.trim().to_string(),
                user_id: req.user_id.trim().to_string(),
                base_url,
            };
            *state.last_agent.lock().await = req.agent.clone();
            start_channel(&app, &state, creds, req.agent).await;
        }
    }
    weixin_channel_status(state).await
}

#[tauri::command]
pub async fn weixin_channel_restart(
    app: AppHandle,
    state: State<'_, WeixinState>,
) -> Result<WeixinStatus, String> {
    {
        let _lock = state.apply_lock.lock().await;
        let applied = state.applied.lock().await.clone();
        let Some(creds) = applied else {
            return Err("微信通道未启用".into());
        };
        let agent = state.last_agent.lock().await.clone();
        start_channel(&app, &state, creds, agent).await;
    }
    weixin_channel_status(state).await
}

#[tauri::command]
pub fn weixin_protect_secret(plain: String) -> Result<String, String> {
    crate::secret_protect::protect_secret(&plain)
}

#[tauri::command]
pub fn weixin_reveal_secret(stored: String) -> Result<String, String> {
    crate::secret_protect::unprotect_secret(stored.trim())
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalize_modes() {
        assert_eq!(crate::wecom_channel::normalize_answer_mode("linked"), "linked");
    }
}
