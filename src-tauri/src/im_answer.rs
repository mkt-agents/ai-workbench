//! 轻量 IM 问答引擎：联动检索 + 天玑 agent_run；审批降级为文字「批准/拒绝」。

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::sync::{oneshot, Mutex};

use crate::agent::{
    agent_decide, subscribe_agent_bridge, unsubscribe_agent_bridge, AgentBridgeEvent, AgentRunRequest,
};
use crate::wecom_orchestrator::WecomAgentConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImTurnRequest {
    pub request_id: String,
    pub user_text: String,
    pub where_from: String,
    pub answer_mode: String,
    pub doc_dir: String,
    pub agent: WecomAgentConfig,
}

#[derive(Debug, Clone)]
pub struct ImTurnResult {
    pub text: String,
}

pub(crate) struct LiveApproval {
    request_id: String,
    call_ids: Vec<String>,
    reply: oneshot::Sender<bool>,
}

/// Channel holds this so the next inbound 「批准/拒绝」 can unblock the running turn.
pub type ApprovalSlot = Arc<Mutex<Option<LiveApproval>>>;

pub fn new_approval_slot() -> ApprovalSlot {
    Arc::new(Mutex::new(None))
}

/// Returns Some(ack) if the text was an approval decision for a live turn.
pub async fn try_resolve_approval(slot: &ApprovalSlot, user_text: &str) -> Option<String> {
    let t = user_text.trim();
    let decision = if t == "批准" || t.eq_ignore_ascii_case("approve") || t == "1" {
        Some(true)
    } else if t == "拒绝" || t.eq_ignore_ascii_case("deny") || t == "2" || t == "0" {
        Some(false)
    } else {
        None
    }?;
    let live = {
        let mut g = slot.lock().await;
        g.take()
    }?;
    let _ = agent_decide(
        live.request_id,
        live.call_ids,
        decision,
        None,
        None,
    );
    let _ = live.reply.send(decision);
    Some(if decision {
        "已批准，继续执行…".into()
    } else {
        "已拒绝该操作。".into()
    })
}

pub async fn run_turn(
    app: AppHandle,
    req: ImTurnRequest,
    slot: ApprovalSlot,
    mut on_partial: impl FnMut(String) + Send + 'static,
) -> ImTurnResult {
    {
        let mut g = slot.lock().await;
        *g = None;
    }

    let answer_mode = crate::wecom_channel::normalize_answer_mode(&req.answer_mode);
    let doc_dir = if req.doc_dir.trim().is_empty() {
        "doc".to_string()
    } else {
        req.doc_dir.trim().to_string()
    };

    let cross_block = {
        let plan = crate::wecom_cross_query::plan_for(&req.user_text, &answer_mode);
        let repo = req.agent.repo_path.clone();
        let dd = doc_dir.clone();
        let ask = req.user_text.clone();
        let rid = req.request_id.clone();
        let bundle = tokio::task::spawn_blocking(move || {
            crate::wecom_cross_query::build_bundle(&plan, &repo, &dd, &ask, "", 0)
        })
        .await
        .unwrap_or_default();
        let snap = bundle.to_snapshot(&rid, "weixin");
        crate::wecom_diagnostics::record(
            &rid,
            "cross_query",
            "ok",
            None,
            None,
            serde_json::json!({
                "docHits": snap.doc_hits,
                "codeHits": snap.code_hits,
                "logHits": snap.log_hits,
                "summary": snap.summary,
                "linked": snap.linked,
            }),
        );
        crate::wecom_cross_query::note_latest(snap);
        let text = bundle.format_context();
        if text.is_empty() {
            String::new()
        } else {
            let clipped: String = text.chars().take(10_000).collect();
            format!("\n宿主已预取的联动上下文（优先使用，不足再查工具）：\n{clipped}\n")
        }
    };

    let mut permission = req.agent.permission.clone();
    if permission.trim().is_empty() {
        permission = "read".into();
    }

    let system = format!(
        "你是 AI Workbench 的天玑助手，正在通过{where}回答。回复简洁、适合即时通讯；少用大段代码围栏。\n{cross}",
        where = req.where_from,
        cross = cross_block,
    );

    let agent_req = AgentRunRequest {
        config: req.agent.config,
        fallback_config: req.agent.fallback_config,
        system,
        user: String::new(),
        request_id: req.request_id.clone(),
        repo_path: req.agent.repo_path,
        services: req.agent.services,
        history: Vec::new(),
        messages: vec![crate::agent_protocol::Message::text("user", &req.user_text)],
        mode: req.agent.mode,
        apps: Vec::new(),
        permission,
        tools: req.agent.tools,
        vision: req.agent.vision,
        diff_confirm: req.agent.diff_confirm,
        always_allow: req.agent.always_allow,
        web_allow: req.agent.web_allow,
        web_tier: req.agent.web_tier,
        web_search_provider: req.agent.web_search_provider,
        web_search_key: req.agent.web_search_key,
        subagents: req.agent.subagents,
        subagent_write: false,
        mcp: req.agent.mcp,
        parallel_reads: req.agent.parallel_reads,
        retries: req.agent.retries,
        grep_ignore: req.agent.grep_ignore,
        plan_first: req.agent.plan_first,
        context_tokens: req.agent.context_tokens,
        result_cap: req.agent.result_cap,
        anthropic_stream: req.agent.anthropic_stream,
        mcp_enabled: req.agent.mcp_enabled,
        document_git: true,
    };

    let mut rx = subscribe_agent_bridge(&req.request_id);
    struct BridgeGuard(String);
    impl Drop for BridgeGuard {
        fn drop(&mut self) {
            unsubscribe_agent_bridge(&self.0);
        }
    }
    let _guard = BridgeGuard(req.request_id.clone());

    let request_id = req.request_id.clone();
    let slot_pump = Arc::clone(&slot);
    let pump = tauri::async_runtime::spawn(async move {
        let mut acc = String::new();
        let watchdog = Duration::from_secs(360);
        loop {
            let next = tokio::time::timeout(watchdog, rx.recv()).await;
            let ev = match next {
                Ok(ev) => ev,
                Err(_) => {
                    crate::cancellation::cancel_request(&request_id);
                    break "**本轮已超时**，未完成的模型正文不展示。".to_string();
                }
            };
            match ev {
                Some(AgentBridgeEvent::Chunk(t)) => {
                    acc.push_str(&t);
                }
                Some(AgentBridgeEvent::ApprovalNeeded {
                    call_ids,
                    summary,
                    can_remember: _,
                }) => {
                    let (tx, rx_dec) = oneshot::channel();
                    {
                        let mut g = slot_pump.lock().await;
                        *g = Some(LiveApproval {
                            request_id: request_id.clone(),
                            call_ids: call_ids.clone(),
                            reply: tx,
                        });
                    }
                    let tip = format!(
                        "🔐 需要确认：{summary}\n回复「批准」同意，回复「拒绝」取消（5 分钟内有效）。"
                    );
                    on_partial(tip);
                    let decided = tokio::time::timeout(Duration::from_secs(300), rx_dec).await;
                    match decided {
                        Ok(Ok(true)) => {
                            // agent_decide already called by try_resolve_approval
                            continue;
                        }
                        Ok(Ok(false)) => {
                            continue;
                        }
                        _ => {
                            let _ = agent_decide(
                                request_id.clone(),
                                call_ids,
                                false,
                                None,
                                None,
                            );
                            {
                                let mut g = slot_pump.lock().await;
                                *g = None;
                            }
                            if acc.is_empty() {
                                acc = "审批超时，已自动拒绝。".into();
                            } else {
                                acc.push_str("\n\n审批超时，已自动拒绝。");
                            }
                            crate::cancellation::cancel_request(&request_id);
                            break acc;
                        }
                    }
                }
                Some(AgentBridgeEvent::Done { .. }) => {
                    if acc.is_empty() {
                        acc = "（无文本回复）".into();
                    }
                    break acc;
                }
                Some(AgentBridgeEvent::Err(e)) => {
                    break format!("**本轮中断**：{e}");
                }
                None => break "**响应意外结束**".into(),
            }
        }
    });

    let run = crate::agent::agent_run(app, agent_req).await;
    let text = pump.await.unwrap_or_else(|_| "回合内部错误".into());
    {
        let mut g = slot.lock().await;
        *g = None;
    }
    if let Err(e) = run {
        if text.starts_with("**") {
            return ImTurnResult { text };
        }
        return ImTurnResult {
            text: format!("**本轮失败**：{e}"),
        };
    }
    ImTurnResult { text }
}

#[cfg(test)]
mod tests {
    #[test]
    fn approval_words() {
        assert_eq!("批准".trim(), "批准");
        assert_eq!("拒绝".trim(), "拒绝");
    }
}
