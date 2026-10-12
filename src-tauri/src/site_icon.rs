//! 网页工具「我的站点」图标：由后端代取。
//!
//! 页面里直接 `<img src="{origin}/favicon.ico">` 不可靠：不少站点对跨站图片请求直接报错
//! （sojson、clawhub），有的 `/favicon.ico` 回的是 HTML（云效），图标实际声明在首页
//! `<link rel="icon">` 里。这里按清晰度挑页面声明的图标（16px 的 favicon 在 2× 屏会糊），
//! 再回落约定路径，按文件头校验确实是图片后以 data URL 返回，并落盘缓存（命中 7 天、取不到 1 天后重试）。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use base64::Engine;
use url::Url;

const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const MAX_ICON_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 1024 * 1024;
const MAX_CANDIDATES: usize = 6;
const HIT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MISS_TTL: Duration = Duration::from_secs(24 * 3600);

/// 读标签里的属性值（大小写不敏感；支持双引号 / 单引号 / 无引号）。
fn tag_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(off) = lower[from..].find(&needle) {
        let at = from + off;
        from = at + needle.len();
        if at > 0 && !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &tag[at + needle.len()..];
        let value = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?,
            _ => rest.split(|c: char| c.is_ascii_whitespace() || c == '>').next()?,
        };
        return Some(value.replace("&amp;", "&").trim().to_string());
    }
    None
}

/// 估算图标边长（px），用来挑清晰的：SVG 与 `sizes="any"` 视为无限大；
/// 没写 sizes 时 apple-touch-icon 按惯例 180、`.ico` 按 16、其余按 32。
fn icon_score(tag: &str, href: &str, is_touch: bool) -> u32 {
    let path = href.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let ty = tag_attr(tag, "type").unwrap_or_default().to_ascii_lowercase();
    if path.ends_with(".svg") || ty.contains("svg") {
        return u32::MAX;
    }
    if let Some(sizes) = tag_attr(tag, "sizes").map(|s| s.to_ascii_lowercase()) {
        if sizes.split_whitespace().any(|s| s == "any") {
            return u32::MAX - 1;
        }
        let best = sizes
            .split_whitespace()
            .filter_map(|s| s.split('x').next()?.parse::<u32>().ok())
            .max();
        if let Some(px) = best {
            return px;
        }
    }
    if is_touch {
        180
    } else if path.ends_with(".ico") {
        16
    } else {
        32
    }
}

/// 页面里声明的图标地址，按清晰度从高到低（同分保持文档顺序）。
/// `mask-icon` 是单色轮廓，不要。
pub fn icon_links(html: &str, base: &Url) -> Vec<Url> {
    scored_icon_links(html, base).into_iter().map(|(_, u)| u).collect()
}

fn scored_icon_links(html: &str, base: &Url) -> Vec<(u32, Url)> {
    let lower = html.to_ascii_lowercase();
    let mut found: Vec<(u32, Url)> = Vec::new();
    let mut from = 0;
    while let Some(off) = lower[from..].find("<link") {
        let start = from + off;
        let Some(len) = lower[start..].find('>') else { break };
        let tag = &html[start..start + len + 1];
        from = start + len + 1;
        let Some(rel) = tag_attr(tag, "rel").map(|r| r.to_ascii_lowercase()) else { continue };
        let rels: Vec<&str> = rel.split_whitespace().collect();
        let is_touch = rels.iter().any(|r| r.starts_with("apple-touch-icon"));
        if !is_touch && !rels.contains(&"icon") {
            continue;
        }
        let Some(href) = tag_attr(tag, "href").filter(|h| !h.is_empty()) else { continue };
        let Ok(url) = base.join(&href) else { continue };
        if !matches!(url.scheme(), "http" | "https" | "data") {
            continue;
        }
        found.push((icon_score(tag, &href, is_touch), url));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found
}

/// 低于这个边长的图标在 2× 屏的 24px 槽位里会发糊。
const SHARP_PX: u32 = 64;

/// 候选顺序：清晰的声明图标 → 约定路径 `/apple-touch-icon.png` → 低清声明图标 → `/favicon.ico`。
pub fn icon_candidates(html: Option<&str>, page: &Url) -> Vec<Url> {
    let declared = html.map(|h| scored_icon_links(h, page)).unwrap_or_default();
    let split = declared.iter().position(|(s, _)| *s < SHARP_PX).unwrap_or(declared.len());
    let mut out: Vec<Url> = Vec::new();
    let mut push = |u: Url| {
        if !out.contains(&u) {
            out.push(u);
        }
    };
    for (_, u) in &declared[..split] {
        push(u.clone());
    }
    if let Ok(u) = page.join("/apple-touch-icon.png") {
        push(u);
    }
    for (_, u) in &declared[split..] {
        push(u.clone());
    }
    if let Ok(u) = page.join("/favicon.ico") {
        push(u);
    }
    out
}

/// 按文件头判断图片类型；不信 Content-Type（云效的 `/favicon.ico` 200 回的是 HTML）。
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0, 0, 1, 0]) {
        return Some("image/x-icon");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
        return Some("image/svg+xml");
    }
    None
}

/// 缓存文件名：只留 host 与端口里的安全字符。
fn cache_key(page: &Url) -> Option<String> {
    let host = page.host_str()?;
    let key = match page.port() {
        Some(p) => format!("{host}_{p}"),
        None => host.to_string(),
    };
    Some(key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect())
}

fn cache_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("APPDATA").ok()?)
        .join("com.ai-workbench.app")
        .join("site-icons");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn fresh(path: &PathBuf, ttl: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl)
}

async fn fetch_bytes(client: &reqwest::Client, url: &Url, limit: usize) -> Option<Vec<u8>> {
    let resp = client.get(url.as_str()).send().await.ok()?;
    if !resp.status().is_success() || resp.content_length().unwrap_or(0) as usize > limit {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    (bytes.len() <= limit && !bytes.is_empty()).then(|| bytes.to_vec())
}

fn data_url_of(bytes: &[u8]) -> Option<String> {
    let mime = sniff_image_mime(bytes)?;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

async fn discover(page: &Url) -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    let html = fetch_bytes(&client, page, MAX_HTML_BYTES)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    for url in icon_candidates(html.as_deref(), page).into_iter().take(MAX_CANDIDATES) {
        if url.scheme() == "data" {
            if url.as_str().starts_with("data:image/") {
                return Some(url.to_string());
            }
            continue;
        }
        if let Some(found) = fetch_bytes(&client, &url, MAX_ICON_BYTES).await.and_then(|b| data_url_of(&b)) {
            return Some(found);
        }
    }
    None
}

/// 取站点图标（data URL）；取不到回 `None`，前端显示首字母色块。
#[tauri::command]
pub async fn fetch_site_icon(url: String) -> Result<Option<String>, String> {
    let page = Url::parse(url.trim()).map_err(|_| "地址无效".to_string())?;
    if !matches!(page.scheme(), "http" | "https") {
        return Ok(None);
    }
    let dir = cache_dir();
    let key = cache_key(&page);
    // 文件名带版本：挑选规则变了（v2 起优先高清）就换前缀，旧缓存自然作废。
    let paths = dir.zip(key).map(|(d, k)| (d.join(format!("{k}.v2.txt")), d.join(format!("{k}.v2.miss"))));
    if let Some((hit, miss)) = &paths {
        if fresh(hit, HIT_TTL) {
            if let Ok(s) = fs::read_to_string(hit) {
                return Ok(Some(s));
            }
        }
        if fresh(miss, MISS_TTL) {
            return Ok(None);
        }
    }
    let found = discover(&page).await;
    if let Some((hit, miss)) = &paths {
        match &found {
            Some(data) => {
                let _ = fs::write(hit, data);
                let _ = fs::remove_file(miss);
            }
            None => {
                let _ = fs::write(miss, b"");
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://devops.console.aliyun.com/workbench").unwrap()
    }

    #[test]
    fn icon_links_resolve_relative_and_order_touch_last() {
        let html = r#"<head>
            <link rel="apple-touch-icon" sizes="180x180" href="/apple.png">
            <LINK REL='shortcut icon' HREF='//img.alicdn.com/fav.ico'>
            <link rel="mask-icon" href="/mask.svg">
            <link rel="stylesheet" href="/a.css">
            <link href="icons/32.png?v=1&amp;x=2" rel=icon sizes=32x32>
        </head>"#;
        let links: Vec<String> = icon_links(html, &base()).into_iter().map(|u| u.to_string()).collect();
        assert_eq!(
            links,
            vec![
                "https://devops.console.aliyun.com/apple.png",
                "https://devops.console.aliyun.com/icons/32.png?v=1&x=2",
                "https://img.alicdn.com/fav.ico",
            ]
        );
    }

    #[test]
    fn svg_and_large_sizes_win() {
        let html = r#"<link rel="icon" href="/16.png" sizes="16x16">
            <link rel="icon" href="/192.png" sizes="32x32 192x192">
            <link rel="icon" type="image/svg+xml" href="/logo">"#;
        let links: Vec<String> = icon_links(html, &base()).into_iter().map(|u| u.path().to_string()).collect();
        assert_eq!(links, vec!["/logo", "/192.png", "/16.png"]);
    }

    #[test]
    fn conventional_touch_icon_goes_before_blurry_declared_ones() {
        let html = r#"<link rel="icon" href="/big.png" sizes="128x128"><link rel="shortcut icon" href="/f.ico">"#;
        let c: Vec<String> = icon_candidates(Some(html), &base()).into_iter().map(|u| u.path().to_string()).collect();
        assert_eq!(c, vec!["/big.png", "/apple-touch-icon.png", "/f.ico", "/favicon.ico"]);
        let none: Vec<String> = icon_candidates(None, &base()).into_iter().map(|u| u.path().to_string()).collect();
        assert_eq!(none, vec!["/apple-touch-icon.png", "/favicon.ico"]);
    }

    #[test]
    fn icon_links_ignore_rel_substrings_and_empty_href() {
        let html = r#"<link rel="iconic" href="/x.png"><link data-rel="icon" rel="preload" href="/y.png"><link rel="icon" href="">"#;
        assert!(icon_links(html, &base()).is_empty());
    }

    #[test]
    fn sniff_accepts_real_images_and_rejects_html() {
        assert_eq!(sniff_image_mime(&[0, 0, 1, 0, 1, 0]), Some("image/x-icon"));
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(sniff_image_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"\xEF\xBB\xBF <svg xmlns='x'/>".as_slice()), Some("image/svg+xml"));
        assert_eq!(sniff_image_mime(b"<?xml version='1.0'?><svg/>"), Some("image/svg+xml"));
        assert_eq!(sniff_image_mime(b"<!DOCTYPE html><html>"), None);
    }

    /// 平时不跑：`cargo test -- --ignored audit_real_site_icons --nocapture`
    #[tokio::test]
    #[ignore = "访问真实站点"]
    async fn audit_real_site_icons() {
        for site in [
            "https://www.sojson.com/",
            "https://clawhub.ai/",
            "https://devops.console.aliyun.com/",
            "https://beian.miit.gov.cn/",
            "https://cursor.com/",
        ] {
            let found = discover(&Url::parse(site).unwrap()).await;
            let px = found.as_deref().and_then(|d| {
                let b = base64::engine::general_purpose::STANDARD.decode(d.split(',').nth(1)?).ok()?;
                if b.starts_with(&[0x89, b'P']) {
                    Some(u32::from_be_bytes(b[16..20].try_into().ok()?))
                } else if b.starts_with(&[0, 0, 1, 0]) {
                    let n = u16::from_le_bytes([b[4], b[5]]) as usize;
                    (0..n).map(|i| match b[6 + i * 16] { 0 => 256, w => w as u32 }).max()
                } else {
                    None
                }
            });
            println!("{site} -> {} px={px:?}", found.as_deref().map(|d| &d[..d.len().min(30)]).unwrap_or("None"));
        }
    }

    #[test]
    fn cache_key_is_filesystem_safe() {
        let u = Url::parse("http://localhost:8080/a?b").unwrap();
        assert_eq!(cache_key(&u).as_deref(), Some("localhost_8080"));
        assert_eq!(cache_key(&base()).as_deref(), Some("devops.console.aliyun.com"));
    }
}
