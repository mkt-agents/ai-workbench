//! 企微业务 CLI：文档 / 日程 / 智能表格，复用机器人通道的 Bot ID/Secret。
//!
//! 走企微 Open API（`qyapi.weixin.qq.com/cgi-bin`）。注意：这些业务接口通常要求
//! 「自建应用」的 corpid+secret（设置页「业务 API Corpid / 应用 Secret」，选填）；
//! 机器人 Bot ID/Secret 不能当 corpid 用（企微回 40013）。调用失败会原样回传
//! `errcode`/`errmsg` 并附可操作提示——不静默吞掉，也不假装成功。
//!
//! 工具清单（名字与 `agent_tools.rs` / `agent.rs::tool_specs` 三处保持一致）：
//! - `wecom_doc_list` / `wecom_doc_create` / `wecom_doc_read`
//! - `wecom_calendar_list` / `wecom_calendar_create`
//! - `wecom_sheet_list` / `wecom_sheet_create` / `wecom_sheet_add_row`

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::wecom_channel::{AppliedCreds, WecomState};

const QYAPI: &str = "https://qyapi.weixin.qq.com/cgi-bin";
/// token 有效期 7200s，缓存 7000s 提前刷新
const TOKEN_TTL: Duration = Duration::from_secs(7000);

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(reqwest::Client::new)
}

/// 入口：被 `agent.rs::execute_tool` 按工具名分发到这里。
pub async fn run(app: AppHandle, name: &str, args: &Value) -> Result<String, String> {
    let state = app.state::<WecomState>();
    let creds = state.applied.lock().await.clone();
    let creds = creds.ok_or_else(|| {
        "企微通道未配置：请先在「设置 → 联网扩展 → 企业微信机器人」中保存 Bot ID/Secret 并启用。".to_string()
    })?;

    let token = token_for(&state, &creds).await?;
    let (path, body) = build(name, args)?;
    let url = format!("{QYAPI}/{path}?access_token={token}");

    let resp = client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("企微请求失败：{e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("企微响应解析失败：{e}"))?;

    match v.get("errcode").and_then(|c| c.as_i64()) {
        Some(0) | None => Ok(format!(
            "{} 成功：\n{}",
            name,
            serde_json::to_string_pretty(&v).unwrap_or_default()
        )),
        Some(code) => Err(format!(
            "企微返回错误 errcode={} errmsg={}",
            code,
            v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
        )),
    }
}

/// 取 access_token：优先用自建应用 Corpid/Secret（业务 API 的正规凭证）；
/// 未配置时回落机器人凭证——企微大概率回 40013，错误里给出可操作提示。
async fn token_for(state: &WecomState, creds: &AppliedCreds) -> Result<String, String> {
    let (corpid, secret, from_corp) = match (
        creds.corp_id.as_deref(),
        creds.corp_secret.as_deref(),
    ) {
        (Some(id), Some(sec)) if !id.is_empty() && !sec.is_empty() => (id, sec, true),
        _ => (creds.bot_id.as_str(), creds.secret.as_str(), false),
    };
    {
        if let Some((t, at)) = &*state.api_token.lock().await {
            if at.elapsed() < TOKEN_TTL {
                return Ok(t.clone());
            }
        }
    }
    let url = format!("{QYAPI}/gettoken?corpid={corpid}&corpsecret={secret}");
    let resp = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("获取企微 access_token 失败：{e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("token 响应解析失败：{e}"))?;
    let code = v.get("errcode").and_then(|c| c.as_i64()).unwrap_or(0);
    if code != 0 {
        let hint = if from_corp {
            "自建应用 Corpid/Secret 被企微拒绝：请核对 Corpid 是否完整、应用 Secret 是否被重置，以及本机 IP 是否在应用的「企业可信 IP」内".to_string()
        } else {
            "机器人 Bot ID/Secret 不能调用文档/日程/智能表格等业务接口（企微要求自建应用凭证）。请在「设置 → 联网扩展 → 企业微信机器人」中填写「业务 API Corpid / 应用 Secret」后重试".to_string()
        };
        return Err(format!(
            "企微获取 token 失败 errcode={} errmsg={}。{}",
            code,
            v.get("errmsg").and_then(|m| m.as_str()).unwrap_or(""),
            hint
        ));
    }
    let token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or("企微未返回 access_token")?
        .to_string();
    *state.api_token.lock().await = Some((token.clone(), std::time::Instant::now()));
    Ok(token)
}

/// 把工具名 + 参数映射到 (接口 path, 请求体)。
fn build(name: &str, args: &Value) -> Result<(String, Value), String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| args.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let ok = match name {
        // —— 文档（wedoc）——
        "wecom_doc_create" => (
            "wedoc/create_doc",
            json!({
                "doc_type": if s("doc_type").is_empty() { 3 } else { n("doc_type") },
                "doc_name": s("title"),
            }),
        ),
        "wecom_doc_list" => (
            "wedoc/doc_search",
            json!({ "doc_name": s("query"), "need_search": true }),
        ),
        "wecom_doc_read" => ("wedoc/doc_get", json!({ "docid": s("docid") })),
        // —— 日程（oa/calendar + oa/schedule）——
        "wecom_calendar_list" => ("oa/calendar/get", json!({})),
        "wecom_calendar_create" => (
            "oa/schedule/add",
            json!({
                "schedule": {
                    "calendar_id": s("calendar_id"),
                    "summary": s("title"),
                    "start_time": n("start"),
                    "end_time": n("end"),
                    "attendees": if s("attendees").is_empty() {
                        Vec::<Value>::new()
                    } else {
                        s("attendees")
                            .split(',')
                            .map(|x| json!({ "userid": x.trim() }))
                            .collect::<Vec<_>>()
                    }
                }
            }),
        ),
        // —— 智能表格（近似 wedoc 的表格类 doc）——
        "wecom_sheet_list" => (
            "wedoc/doc_search",
            json!({ "doc_name": s("query"), "doc_type": 10 }),
        ),
        "wecom_sheet_create" => (
            "wedoc/create_doc",
            json!({ "doc_type": 10, "doc_name": s("title") }),
        ),
        "wecom_sheet_add_row" => (
            "wedoc/smartsheet/add_rows",
            json!({
                "docid": s("sheet_id"),
                "sheet_id": s("sheet_id"),
                "rows": parse_rows(&s("fields"))?,
            }),
        ),
        _ => return Err(format!("未知企微业务工具：{name}")),
    };
    // 路径是 &str，统一成 String 以匹配返回类型
    Ok((ok.0.to_string(), ok.1))
}

/// `fields` 是 JSON 数组（每项是一行单元格），解析失败给明确报错。
fn parse_rows(raw: &str) -> Result<Vec<Value>, String> {
    if raw.trim().is_empty() {
        return Err("缺少 fields：传一个 JSON 数组，如 [{\"标题\":\"买菜\",\"状态\":\"待办\"}]".into());
    }
    serde_json::from_str::<Vec<Value>>(raw).map_err(|e| format!("fields 不是合法 JSON 数组：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rows_rejects_empty() {
        assert!(parse_rows("").is_err());
    }

    #[test]
    fn parse_rows_accepts_array() {
        let v = parse_rows("[{\"a\":1}]").unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn build_maps_known_tools() {
        for name in [
            "wecom_doc_list",
            "wecom_doc_create",
            "wecom_doc_read",
            "wecom_calendar_list",
            "wecom_calendar_create",
            "wecom_sheet_list",
            "wecom_sheet_create",
            "wecom_sheet_add_row",
        ] {
            // sheet_add_row 需要合法 fields，其它工具空参数即可映射端点
            let args = if name == "wecom_sheet_add_row" {
                json!({ "sheet_id": "x", "fields": "[{\"a\":1}]" })
            } else {
                json!({})
            };
            assert!(build(name, &args).is_ok(), "{name} 应可映射");
        }
        assert!(build("wecom_unknown", &json!({})).is_err());
    }
}
