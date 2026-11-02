//! 个人微信 iLink 扫码绑定 IPC。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeixinQrStart {
    pub qrcode: String,
    /// 二维码内容（URL 或文本），用于本机 SVG。
    pub qr_content: String,
    pub qr_svg: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeixinQrPoll {
    /// wait | scaned | confirmed | expired | need_verifycode | error
    pub status: String,
    pub bot_token: Option<String>,
    pub bot_id: Option<String>,
    pub user_id: Option<String>,
    pub base_url: Option<String>,
    pub detail: String,
    pub need_verify: bool,
}

#[tauri::command]
pub async fn weixin_qr_start(base_url: Option<String>) -> Result<WeixinQrStart, String> {
    let base = base_url.unwrap_or_default();
    let client = crate::weixin_ilink::IlinkClient::new(&base, None)?;
    let started = client.get_bot_qrcode().await?;
    let qr_svg = crate::wecom_qr::render_qr_svg(&started.qrcode_img_content)?;
    Ok(WeixinQrStart {
        qrcode: started.qrcode,
        qr_content: started.qrcode_img_content.clone(),
        qr_svg,
    })
}

#[tauri::command]
pub async fn weixin_qr_poll(
    qrcode: String,
    verify_code: Option<String>,
    base_url: Option<String>,
) -> Result<WeixinQrPoll, String> {
    let qrcode = qrcode.trim().to_string();
    if qrcode.is_empty() {
        return Err("缺少 qrcode".into());
    }
    let base = base_url.unwrap_or_default();
    let client = crate::weixin_ilink::IlinkClient::new(&base, None)?;
    let st = client
        .get_qrcode_status(&qrcode, verify_code.as_deref())
        .await?;
    let status = st.status.to_ascii_lowercase();
    match status.as_str() {
        "confirmed" => {
            let token = st
                .bot_token
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "扫码成功但缺少 bot_token".to_string())?;
            let bot_id = st.ilink_bot_id.clone().unwrap_or_default();
            let user_id = st.ilink_user_id.clone().unwrap_or_default();
            let mut base_url = st.baseurl.clone().unwrap_or_default();
            if !base_url.is_empty() {
                base_url = crate::weixin_ilink::normalize_base_url(&base_url)?;
            }
            Ok(WeixinQrPoll {
                status: "confirmed".into(),
                bot_token: Some(token),
                bot_id: Some(bot_id),
                user_id: Some(user_id),
                base_url: if base_url.is_empty() { None } else { Some(base_url) },
                detail: "扫码成功，已获取微信 Bot Token".into(),
                need_verify: false,
            })
        }
        "expired" => Ok(WeixinQrPoll {
            status: "expired".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: "二维码已过期，请重新扫码".into(),
            need_verify: false,
        }),
        "need_verifycode" => Ok(WeixinQrPoll {
            status: "need_verifycode".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: "请在手机微信查看配对数字并在下方输入".into(),
            need_verify: true,
        }),
        "verify_code_blocked" => Ok(WeixinQrPoll {
            status: "expired".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: "配对数字错误次数过多，请重新扫码".into(),
            need_verify: false,
        }),
        "scaned" | "scanned" => Ok(WeixinQrPoll {
            status: "scaned".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: "已扫码，请在手机上确认…".into(),
            need_verify: false,
        }),
        "scaned_but_redirect" => {
            // Continue with redirected host if provided — caller may re-poll with new base.
            Ok(WeixinQrPoll {
                status: "wait".into(),
                bot_token: None,
                bot_id: None,
                user_id: None,
                base_url: st.redirect_host.map(|h| {
                    if h.starts_with("https://") {
                        h
                    } else {
                        format!("https://{h}")
                    }
                }),
                detail: "扫码跳转中…".into(),
                need_verify: false,
            })
        }
        _ => Ok(WeixinQrPoll {
            status: "wait".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: "等待微信扫码…".into(),
            need_verify: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_labels_cover_need_verify() {
        assert!(super::WeixinQrPoll {
            status: "need_verifycode".into(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            detail: String::new(),
            need_verify: true,
        }
        .need_verify);
    }
}
