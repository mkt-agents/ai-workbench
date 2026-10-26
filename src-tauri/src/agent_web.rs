//! `web_fetch`: pull a page's text into the conversation.
//!
//! Deliberately narrow. Only `https://`, only hosts on the user's allowlist (empty =
//! nothing), never a loopback / private address, and the body is flattened to text before
//! it reaches the model. Redirects are followed by hand, one hop at a time, because a 302
//! pointing at another host is the classic way around a host allowlist — every hop is
//! re-checked with the same [`url_allowed`] before it is requested. `web_search` lives here
//! too, but only runs when the user configured a provider + API key.

use std::net::IpAddr;
use std::time::Duration;

/// Characters of text returned to the model; the rest is cut with a note.
const TEXT_CAP: usize = 8_000;
/// Largest body we are willing to download.
const BODY_CAP: u64 = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
/// Redirects followed per fetch. Each hop is re-validated against the allowlist, which is
/// the real guard — the hop limit only stops loops.
const MAX_REDIRECTS: usize = 3;
/// Elements whose contents are never readable text.
const RAW_TEXT: &[&str] = &["script", "style", "noscript", "template", "svg"];
/// Elements that end a line of prose.
const BLOCK: &[&str] = &[
    "p", "div", "br", "li", "tr", "td", "th", "section", "article", "header", "footer", "main", "nav", "pre",
    "blockquote", "table", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "ul", "ol", "dl", "dt", "dd",
];

/// Percent-free, structure-free text of an HTML document.
pub(crate) fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(html.len() / 2);
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let name = tag_name(&lower[i..]);
            if RAW_TEXT.contains(&name.as_str()) {
                let close = format!("</{name}");
                match lower[i..].find(&close) {
                    Some(pos) => {
                        i += pos;
                        // 跳过闭合标签本身
                        i += lower[i..].find('>').map(|p| p + 1).unwrap_or(0);
                        out.push(b'\n');
                    }
                    // 没闭合（截断的页面）：剩下的都别要了
                    None => break,
                }
                continue;
            }
            if BLOCK.contains(&name.as_str()) {
                out.push(b'\n');
            }
            i = match lower[i..].find('>') {
                Some(p) => i + p + 1,
                None => bytes.len(),
            };
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    tidy(&String::from_utf8_lossy(&out))
}

/// `<div class=…>` → `div`; anything that is not a tag name becomes "".
fn tag_name(rest: &str) -> String {
    let mut out = String::new();
    for ch in rest.chars().skip(1) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            break;
        }
    }
    out
}

/// Collapse whitespace, decode the handful of entities that matter, drop blank runs.
fn tidy(text: &str) -> String {
    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let mut lines: Vec<String> = Vec::new();
    for raw in decoded.lines() {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            // 最多留一个空行，防止代码块之间被压成一坨
            if lines.last().map(|l: &String| !l.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(line);
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

fn is_private_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal")
    {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
        }
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local(),
        Err(_) => false,
    }
}

/// `https://`, a public host, and (unless `open`) a host on the allowlist
/// (`*.example.com` and bare `example.com` both accept subdomains). Pure, so the policy
/// is unit-tested. `open` still bans private/loopback hosts.
pub(crate) fn url_allowed(url: &str, allow: &[String]) -> Result<(), String> {
    url_allowed_tier(url, allow, false)
}

pub(crate) fn url_allowed_tier(url: &str, allow: &[String], open: bool) -> Result<(), String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    // 只有 https：http 明文、file://、ftp:// 一律不许
    if !lower.starts_with("https://") {
        return Err("只允许 https:// 链接（http / file / ftp 都不行）".into());
    }
    let rest = &lower["https://".len()..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("链接里没有域名".into());
    }
    // 去掉 userinfo 与端口
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return Err("链接里没有域名".into());
    }
    if is_private_host(host) {
        return Err(format!("{host} 是本机或内网地址，不能抓取"));
    }
    if open {
        return Ok(());
    }
    if allow.is_empty() {
        return Err("还没有允许任何域名：先到 设置 → 天玑 → 网页抓取白名单 里加上要访问的域名".into());
    }
    let ok = allow.iter().any(|entry| {
        let entry = entry.trim().trim_start_matches("*.").to_ascii_lowercase();
        !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
    });
    if !ok {
        return Err(format!("{host} 不在允许的域名列表里（设置 → 天玑 → 网页抓取白名单）"));
    }
    Ok(())
}

/// `Location` header → absolute URL. Handles the shapes servers actually send: absolute,
/// protocol-relative (`//host/x`), root-relative (`/x`), path-relative (`x`, `../x`) and
/// query-only (`?q=1`). `None` when there is nothing usable to follow.
pub(crate) fn resolve_location(base: &str, location: &str) -> Option<String> {
    let loc = location.trim();
    if loc.is_empty() {
        return None;
    }
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(loc.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if loc.starts_with("//") {
        return Some(format!("{scheme}:{loc}"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if loc.starts_with('/') {
        return Some(format!("{scheme}://{authority}{loc}"));
    }
    let plain = base.split(['?', '#']).next().unwrap_or(base);
    // 只换查询串 / 锚点：接在当前文档后面，不接目录
    if loc.starts_with('?') || loc.starts_with('#') {
        return Some(format!("{plain}{loc}"));
    }
    // 相对路径：拼到当前文档所在目录
    let dir_end = plain.rfind('/').map(|i| i + 1).unwrap_or(plain.len());
    Some(format!("{}{}", &plain[..dir_end], loc))
}

pub(crate) async fn web_fetch(url: &str, allow: &[String]) -> Result<String, String> {
    web_fetch_tier(url, allow, false).await
}

pub(crate) async fn web_fetch_tier(url: &str, allow: &[String], open: bool) -> Result<String, String> {
    url_allowed_tier(url, allow, open)?;
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        // 不自动跟随：每一跳都要用白名单重新校验（302 到别的域名是绕过白名单最省事的办法）
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Mozilla/5.0 (compatible; AIWorkbench)")
        .build()
        .map_err(|e| format!("创建请求失败: {e}"))?;
    let mut current = url.trim().to_string();
    let mut hops = 0usize;
    let resp = loop {
        let resp = client.get(&current).send().await.map_err(|e| format!("请求失败: {e}"))?;
        if !resp.status().is_redirection() {
            break resp;
        }
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let next = resolve_location(&current, to)
            .ok_or_else(|| format!("{current} 返回跳转，但 Location 看不懂：{to:?}"))?;
        // 关键的一行：跳转目标也必须过同一道白名单（open 档仍禁私网）
        url_allowed_tier(&next, allow, open)?;
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(format!("跳转超过 {MAX_REDIRECTS} 次，放弃（最后目标是 {next}）"));
        }
        current = next;
    };
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{current} 返回 {status}"));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Some(len) = resp.content_length() {
        if len > BODY_CAP {
            return Err(format!("页面太大（{} KB），只抓 2MB 以内的", len / 1024));
        }
    }
    // 边收边数：`resp.bytes()` 会先把整个响应读进内存，chunked / 无 Content-Length 的
    // 服务器上 2MB 上限完全不生效（一个白名单内的域名就能把进程撑爆）。
    use futures_util::StreamExt;
    let mut stream = std::pin::pin!(resp.bytes_stream());
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("读取响应失败: {e}"))?;
        if bytes.len() as u64 + chunk.len() as u64 > BODY_CAP {
            return Err("页面太大（超过 2MB），只抓 2MB 以内的".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let text = if content_type.contains("html") || raw.trim_start().starts_with('<') {
        html_to_text(&raw)
    } else if content_type.is_empty() || content_type.contains("text") || content_type.contains("json")
        || content_type.contains("xml")
    {
        raw
    } else {
        return Err(format!("这个类型不抓：{}（只抓 html / 纯文本 / json）", content_type));
    };
    let mut text = text.trim().to_string();
    if text.is_empty() {
        return Err("页面里没有可读文字（可能是纯脚本渲染的页面）".into());
    }
    if text.chars().count() > TEXT_CAP {
        text = text.chars().take(TEXT_CAP).collect::<String>();
        text.push_str(&format!("\n\n…（已截断，只保留前 {TEXT_CAP} 字）"));
    }
    Ok(format!("{url}\n\n{text}"))
}

/// Search via Brave or Tavily. Only called when the user configured provider + key.
pub(crate) async fn web_search(
    query: &str,
    count: usize,
    provider: &str,
    api_key: &str,
) -> Result<String, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("缺少 query".into());
    }
    let key = api_key.trim();
    if key.is_empty() {
        return Err("未配置搜索 API key（设置 → 天玑 → 联网扩展）".into());
    }
    let n = count.clamp(1, 10);
    let provider = provider.trim().to_ascii_lowercase();
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("创建请求失败: {e}"))?;
    match provider.as_str() {
        "brave" => {
            let resp = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("Accept", "application/json")
                .header("X-Subscription-Token", key)
                .query(&[("q", q), ("count", &n.to_string())])
                .send()
                .await
                .map_err(|e| format!("Brave 搜索失败: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("Brave 搜索返回 {}", resp.status()));
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            let results = body
                .pointer("/web/results")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if results.is_empty() {
                return Ok("没有搜索结果。".into());
            }
            let mut lines = vec![format!("Brave 搜索「{q}」：")];
            for (i, item) in results.iter().take(n).enumerate() {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let desc = item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                lines.push(format!("{}. {title}\n   {url}\n   {desc}", i + 1));
            }
            Ok(lines.join("\n"))
        }
        "tavily" => {
            let resp = client
                .post("https://api.tavily.com/search")
                .json(&serde_json::json!({
                    "api_key": key,
                    "query": q,
                    "max_results": n,
                    "include_answer": false
                }))
                .send()
                .await
                .map_err(|e| format!("Tavily 搜索失败: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("Tavily 搜索返回 {}", resp.status()));
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            let results = body
                .get("results")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if results.is_empty() {
                return Ok("没有搜索结果。".into());
            }
            let mut lines = vec![format!("Tavily 搜索「{q}」：")];
            for (i, item) in results.iter().take(n).enumerate() {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let desc = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("{}. {title}\n   {url}\n   {desc}", i + 1));
            }
            Ok(lines.join("\n"))
        }
        _ => Err(format!(
            "未知搜索服务商「{provider}」：目前支持 brave / tavily"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn html_to_text_strips_scripts() {
        let html = r#"
<html><head><title>Doc</title><style>body{color:red}</style>
<script>alert("do not read me")</script></head>
<body><h1>标题</h1><p>第一段 <b>粗体</b></p>
<ul><li>一</li><li>二</li></ul>
<pre>code &lt;tag&gt;</pre>
<svg><text>svg text</text></svg>
</body></html>"#;
        let text = html_to_text(html);
        assert!(!text.contains("do not read me"), "{text}");
        assert!(!text.contains("color:red") && !text.contains("svg text"), "{text}");
        assert!(text.contains("标题"));
        assert!(text.contains("第一段 粗体"));
        assert!(text.contains("code <tag>"));
        // 块级元素换行，行内元素不硬拆
        assert!(text.contains("\n一\n二\n"), "{text}");
        assert!(!text.contains('<') || text.contains("code <tag>"));
    }

    #[test]
    fn url_must_be_https_and_allowlisted() {
        let list = allow(&["docs.rs", "*.anthropic.com"]);
        assert!(url_allowed("https://docs.rs/serde/latest/serde/", &list).is_ok());
        assert!(url_allowed("https://api.anthropic.com/v1", &list).is_ok());
        assert!(url_allowed("https://DOCS.RS/x", &list).is_ok());
        // 协议
        assert!(url_allowed("http://docs.rs/x", &list).is_err());
        assert!(url_allowed("file:///C:/Windows/win.ini", &list).is_err());
        // 白名单
        assert!(url_allowed("https://evil.com/x", &list).is_err());
        assert!(url_allowed("https://docs.rs.evil.com/x", &list).is_err());
        // 空白名单 = 全禁
        assert!(url_allowed("https://docs.rs/x", &[]).is_err());
        // 跳转后的地址走同一道校验：白名单不会被 302 绕过
        let jump = resolve_location("https://docs.rs/x/y", "https://evil.com/x").unwrap();
        assert!(url_allowed(&jump, &list).is_err());
        // 内网 / 本机
        for blocked in [
            "https://127.0.0.1/x",
            "https://10.0.0.5/x",
            "https://192.168.1.1/x",
            "https://172.16.0.1/x",
            "https://localhost:8443/x",
            "https://[::1]/x",
        ] {
            assert!(url_allowed(blocked, &allow(&["localhost", "127.0.0.1", "10.0.0.5", "192.168.1.1"])).is_err(), "{blocked}");
        }
    }

    #[test]
    fn location_header_becomes_an_absolute_url() {
        let base = "https://docs.rs/serde/latest/serde/index.html";
        // 绝对地址原样返回
        assert_eq!(
            resolve_location(base, "https://example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        // 协议相对：借当前 scheme
        assert_eq!(
            resolve_location(base, "//example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        // 根相对：只换路径，保留 authority（含端口）
        assert_eq!(resolve_location(base, "/a/b").as_deref(), Some("https://docs.rs/a/b"));
        assert_eq!(
            resolve_location("https://docs.rs:8443/x/y", "/a").as_deref(),
            Some("https://docs.rs:8443/a")
        );
        // 路径相对：接在当前文档所在目录
        assert_eq!(
            resolve_location(base, "next.html").as_deref(),
            Some("https://docs.rs/serde/latest/serde/next.html")
        );
        // base 带查询串时，dir 以 ? 之前的部分为准
        assert_eq!(
            resolve_location("https://docs.rs/x/y?q=1", "z").as_deref(),
            Some("https://docs.rs/x/z")
        );
        // 只换查询串 / 锚点：接在当前文档上，不接目录
        assert_eq!(
            resolve_location("https://docs.rs/x/y", "?page=2").as_deref(),
            Some("https://docs.rs/x/y?page=2")
        );
        // 空的、看不懂的
        assert_eq!(resolve_location(base, "   "), None);
        assert_eq!(resolve_location("docs.rs/x", "/a"), None);
    }
}
