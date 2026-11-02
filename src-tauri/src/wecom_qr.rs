//! 企微智能机器人扫码绑定。
//!
//! 官方接口：
//! 1. `GET https://work.weixin.qq.com/ai/qc/generate?source=...&plat=2`
//!    → `{ data: { scode, auth_url } }`
//! 2. 用企业微信扫 `auth_url`（或打开 `.../ai/qc/gen?source=...&scode=...`）
//! 3. 轮询 `GET https://work.weixin.qq.com/ai/qc/query_result?scode=...`
//!    → `status=success` 时拿到 `bot_info.botid` / `bot_info.secret`
//!
//! 本模块只负责 HTTP；Secret 落盘加密仍走 `wecom_protect_secret`。

use serde::{Deserialize, Serialize};
use std::time::Duration;

const SOURCE: &str = "ai_workbench";
const QR_GENERATE_URL: &str = "https://work.weixin.qq.com/ai/qc/generate";
const QR_QUERY_URL: &str = "https://work.weixin.qq.com/ai/qc/query_result";
const QR_CODE_PAGE: &str = "https://work.weixin.qq.com/ai/qc/gen";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Windows 桌面 = 2（与 wecom-cli / 官方 SDK 一致）。
fn plat_code() -> u8 {
    2
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    data: Option<GenerateData>,
}

#[derive(Debug, Deserialize)]
struct GenerateData {
    scode: Option<String>,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    data: Option<QueryData>,
}

#[derive(Debug, Deserialize)]
struct QueryData {
    status: Option<String>,
    bot_info: Option<BotInfoPayload>,
}

#[derive(Debug, Deserialize)]
struct BotInfoPayload {
    botid: Option<String>,
    secret: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomQrStart {
    pub scode: String,
    /// 二维码内容（用企业微信扫这个 URL）。
    pub auth_url: String,
    /// 官方二维码展示页（可在浏览器打开）。
    pub qr_page_url: String,
    /// 本机生成的 SVG（不再依赖外网二维码服务）。
    pub qr_svg: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomQrPoll {
    /// waiting | success | expired | error
    pub status: String,
    pub bot_id: Option<String>,
    pub secret: Option<String>,
    pub detail: String,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))
}


/// Encode `content` as a self-contained SVG QR (opaque white bg for scan contrast).
pub fn render_qr_svg(content: &str) -> Result<String, String> {
    use qrcode::render::svg;
    use qrcode::QrCode;
    let code = QrCode::new(content.as_bytes()).map_err(|e| format!("二维码生成失败：{e}"))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// 申请一次扫码会话。
#[tauri::command]
pub async fn wecom_qr_start() -> Result<WecomQrStart, String> {
    let client = client()?;
    let url = format!(
        "{QR_GENERATE_URL}?source={SOURCE}&plat={}",
        plat_code()
    );
    let resp: GenerateResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("申请扫码失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("扫码响应解析失败：{e}"))?;
    let data = resp.data.ok_or_else(|| "扫码响应缺少 data".to_string())?;
    let scode = data
        .scode
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "扫码响应缺少 scode".to_string())?;
    let auth_url = data
        .auth_url
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "扫码响应缺少 auth_url".to_string())?;
    let qr_page_url = format!("{QR_CODE_PAGE}?source={SOURCE}&scode={scode}");
    let qr_svg = render_qr_svg(&auth_url)?;
    crate::wecom_log::write("qr", &format!("start scode={}…", &scode[..scode.len().min(8)]));
    Ok(WecomQrStart {
        scode,
        auth_url,
        qr_page_url,
        qr_svg,
    })
}

/// 轮询扫码结果（前端每 3 秒调一次；超时由前端控制）。
#[tauri::command]
pub async fn wecom_qr_poll(scode: String) -> Result<WecomQrPoll, String> {
    let scode = scode.trim().to_string();
    if scode.is_empty() {
        return Err("缺少 scode".into());
    }
    let client = client()?;
    let url = format!("{QR_QUERY_URL}?scode={scode}");
    let resp: QueryResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("轮询扫码失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("轮询响应解析失败：{e}"))?;
    let Some(data) = resp.data else {
        return Ok(WecomQrPoll {
            status: "waiting".into(),
            bot_id: None,
            secret: None,
            detail: "等待扫码…".into(),
        });
    };
    let status = data.status.unwrap_or_default().to_ascii_lowercase();
    match status.as_str() {
        "success" => {
            let info = data.bot_info.ok_or_else(|| "扫码成功但缺少 bot_info".to_string())?;
            let bot_id = info
                .botid
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "扫码成功但缺少 botid".to_string())?;
            let secret = info
                .secret
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "扫码成功但缺少 secret".to_string())?;
            crate::wecom_log::write("qr", &format!("success bot_id={bot_id}"));
            Ok(WecomQrPoll {
                status: "success".into(),
                bot_id: Some(bot_id),
                secret: Some(secret),
                detail: "扫码成功，已获取 Bot ID / Secret".into(),
            })
        }
        "expired" | "timeout" | "fail" | "failed" => Ok(WecomQrPoll {
            status: "expired".into(),
            bot_id: None,
            secret: None,
            detail: "二维码已过期，请重新扫码".into(),
        }),
        "waiting" | "pending" | "scan" | "" => Ok(WecomQrPoll {
            status: "waiting".into(),
            bot_id: None,
            secret: None,
            detail: "等待企业微信扫码…".into(),
        }),
        other => Ok(WecomQrPoll {
            status: "waiting".into(),
            bot_id: None,
            secret: None,
            detail: format!("扫码进行中（{other}）…"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plat_is_windows_desktop() {
        assert_eq!(plat_code(), 2);
    }

    #[test]
    fn render_qr_svg_contains_svg() {
        let svg = render_qr_svg("https://work.weixin.qq.com/ai/qc/gen?scode=abc").unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.len() > 100);
    }

    #[test]
    fn qr_page_contains_scode() {
        let scode = "abc123";
        let page = format!("{QR_CODE_PAGE}?source={SOURCE}&scode={scode}");
        assert!(page.contains("scode=abc123"));
        assert!(page.contains("ai_workbench"));
    }
}
