//! QR login window + credential capture for the WorkBuddy pool.
//!
//! CodeBuddy renders its own QR page, so we do not generate one — we open that
//! page in a first-class webview window, let the user scan with their phone, and
//! fish the resulting credential out of the page. Two capture paths are wired
//! into the injected script (see `SCAN_ADD_FLOW` notes in docs): localStorage
//! sniffing and a fetch/XHR header hook. The value is carried back through a
//! magic navigation host, the same trick `plugin_commands` uses for OAuth popups
//! — the IPC bridge is not guaranteed on runtime-created windows.
//!
//! Capture is best-effort by design: when nothing is found the UI falls back to
//! manual paste, which is always available.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

/// Navigation host the injected script bounces to. Never actually resolved:
/// `on_navigation` returns false.
const CAPTURE_HOST: &str = "aiwb-cb-capture.save";
/// Windows create webviews on the message-loop thread; a long payload close in
/// that same callback is how the earlier quick-ask code deadlocked, so the
/// teardown is deferred by this much.
const CLOSE_DELAY: Duration = Duration::from_millis(300);
/// Navigation URLs have practical ceilings; beyond this we stop pretending the
/// capture is usable and tell the UI to fall back to paste.
const MAX_VALUE_CHARS: usize = 24_000;

pub const EVENT_CAPTURED: &str = "wb-credential-captured";
pub const WINDOW_PREFIX: &str = "wb-cb-login-";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedCredential {
    pub batch_id: String,
    /// "token" | "cookie"
    pub credential_type: String,
    pub credential_raw: String,
    /// Where it came from: "header:<name>" | "storage:<key>" | "cookie".
    pub via: String,
    /// True when the payload hit the size ceiling and cannot be trusted.
    pub truncated: bool,
}

/// Script installed into the login window.
///
/// It must not break the page it is injected into, so everything is guarded and
/// the first hit wins (`sent` latches).
///
/// `__WB_VERIFY_URL__` is substituted by `capture_script_with()` before
/// injection: it turns the adapter's `credentialProbe` URL into an in-page
/// "does this browser session actually work" check. The web usercenter never
/// sends an `Authorization` header (that only happens in miniProgram mode, per
/// its own axios interceptor), so the header hook alone is guaranteed to come
/// back empty — the in-page probe and the response-body scan are the paths that
/// can actually find something.
const CAPTURE_SCRIPT: &str = r#"
(function () {
  if (window.__wbCaptureInstalled) return;
  window.__wbCaptureInstalled = true;
  var HOST = "aiwb-cb-capture.save";
  var VERIFY_URL = __WB_VERIFY_URL__;
  var sent = false;
  var stats = { installed: 1, ticks: 0, calls: 0, auth: 0, bodyScans: 0, tokens: 0, cookieProbe: 0, cookieOk: false, navigated: 0 };
  window.__wbCaptureStats = stats;

  function submit(kind, value, via) {
    if (sent || !value) return;
    sent = true;
    var clipped = value.length > 24000;
    var payload = clipped ? value.slice(0, 24000) : value;
    var q = new URLSearchParams();
    q.set("kind", kind);
    q.set("via", via);
    q.set("truncated", clipped ? "1" : "0");
    q.set("v", payload);
    try { window.top.location.href = "https://" + HOST + "/c?" + q.toString(); } catch (e) {}
  }

  function isJwt(s) {
    return typeof s === "string" && s.split(".").length === 3 && s.length > 40;
  }
  function looksSecret(s) {
    return typeof s === "string" && s.length >= 24 && /^[A-Za-z0-9\-_.~=+\/%:]+$/.test(s);
  }

  // 只在 CodeBuddy 自家域上抓东西。登录页里嵌着 QQ 的票据/统计 iframe
  // （实测 browsertdidticket.m.qq.com 的响应里就带 "token" 字段），不设白名单
  // 第一个 latch 的会是腾讯的设备票据，不是我们的凭证。
  var TRUSTED = /(^|\.)(codebuddy\.cn|copilot\.tencent\.com|lkeap\.cloud\.tencent\.com)$/i;
  function trustedUrl(u) {
    try {
      if (typeof u !== "string" || !u) return false;
      if (u.charAt(0) === "/") return true;
      return TRUSTED.test(new URL(u, location.href).hostname);
    } catch (e) {
      return false;
    }
  }

  // Path 1.5: response bodies. The API-key dialog itself warns the key is only
  // shown once — that is the create/list response, an XHR, and the full key is
  // in it. Bare JWTs are matched via the base64url-of-`{"` prefix (eyJ) so that
  // random base64 blobs in HTML do not fire.
  function scanBody(url, text) {
    if (sent || !trustedUrl(url)) return;
    if (!text || typeof text !== "string" || text.length < 12) return;
    stats.bodyScans += 1;
    var short = String(url || "").replace(/^https?:\/\//, "").slice(0, 60);
    var jwt = text.match(/eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/);
    if (jwt) { stats.tokens += 1; submit("token", jwt[0], "body:" + short); return; }
    var re = /"(access_token|api_key|apiKey|secret|token|key)"\s*:\s*"([^"]{16,256})"/gi;
    var hit;
    while ((hit = re.exec(text))) {
      if (looksSecret(hit[2])) { stats.tokens += 1; submit("token", hit[2], "body:" + short); return; }
    }
  }

  // Path 1: read the auth header the page itself sends. Whatever the page does
  // with its login state, it has to put it on the wire, so this survives the
  // httpOnly case that document.cookie cannot.
  function readHeader(name, value, url) {
    if (sent || !trustedUrl(url) || typeof name !== "string") return;
    if (!/^(authorization|x-auth-token|token|access[-_]?token)$/i.test(name)) return;
    if (typeof value !== "string" || value.length < 12) return;
    submit("token", value.replace(/^(Bearer|Basic)\s+/i, ""), "header:" + name.toLowerCase());
  }
  function readHeaderSet(h, url) {
    if (!h) return;
    if (Array.isArray(h)) { h.forEach(function (kv) { if (kv && kv.length > 1) readHeader(kv[0], kv[1], url); }); return; }
    if (typeof h.forEach === "function") { h.forEach(function (v, k) { readHeader(k, v, url); }); return; }
    if (typeof h === "object") { Object.keys(h).forEach(function (k) { readHeader(k, h[k], url); }); }
  }
  // ---- 协议探针：记下页面自己发出的接口调用 ----
  // 上游真实端点没人告诉我们，但已登录的页面每天都在调它。只留方法/路径/状态码 +
  // 哪些请求头看起来像凭证（值一律不外传），够用来生成 adapter JSON。
  var probe = { page: "", list: [] };
  var MAX_CALLS = 40;
  var AUTH_HEADER_RE = /^(authorization|x-auth-token|token|access[-_]?token|api[-_]?key|x-api-key|cookie|x-csrf-token)$/i;

  function collectAuthNames(h, out) {
    if (!h) return;
    function one(k) {
      if (typeof k === "string" && AUTH_HEADER_RE.test(k) && out.indexOf(k.toLowerCase()) === -1) {
        out.push(k.toLowerCase());
      }
    }
    if (Array.isArray(h)) {
      h.forEach(function (kv) { if (kv && kv.length > 0) one(kv[0]); });
      return;
    }
    if (typeof h.forEach === "function") { h.forEach(function (_v, k) { one(k); }); return; }
    if (typeof h === "object") { Object.keys(h).forEach(one); }
  }

  function noteCall(method, url, status, names) {
    try {
      if (typeof url !== "string" || !url) return;
      if (/^(data:|blob:|file:|about:)/i.test(url)) return;
      if (/\.(js|css|png|jpe?g|gif|svg|woff2?|ico|map)(\?|#|$)/i.test(url)) return;
      if (/(googletagmanager|doubleclick|beacon|analytics|telemetry|collect|status\.gif|gtag)/i.test(url)) return;
      var entry = {
        m: String(method || "GET").toUpperCase().slice(0, 7),
        u: url.slice(0, 200),
        s: Number(status) | 0,
        a: (names || []).slice(0, 6).join(","),
        n: 1
      };
      for (var i = 0; i < probe.list.length; i++) {
        var old = probe.list[i];
        if (old.u === entry.u && old.m === entry.m && old.s === entry.s) {
          old.n += 1;
          old.a = old.a || entry.a;
          return;
        }
      }
      probe.list.push(entry);
      if (probe.list.length > MAX_CALLS) probe.list.shift();
      probe.page = location.href;
      stats.calls += 1;
      if ((names || []).length > 0) stats.auth += 1;
    } catch (e) {}
  }
  window.__wbProbe = probe;

  var nativeFetch = window.fetch;
  if (nativeFetch) {
    window.fetch = function (input, init) {
      var names = [];
      var meth = "GET";
      var target = "";
      try {
        if (typeof input === "string") { target = input; } else if (input && input.url) { target = input.url; }
        if (input && input.headers) { collectAuthNames(input.headers, names); }
        if (init) {
          if (init.headers) collectAuthNames(init.headers, names);
          if (init.method) meth = init.method;
        }
      } catch (e) {}
      var p = nativeFetch.apply(this, arguments);
      try {
        // 凭证抓取照旧走 readHeaderSet（它一命中就 latch 并回传），探针只旁路记录。
        if (input && input.headers) { readHeaderSet(input.headers, target); }
        if (init && init.headers) { readHeaderSet(init.headers, target); }
        p.then(function (res) {
          noteCall(meth, (res && res.url) || target, res && res.status, names);
          try {
            if (res && res.status >= 200 && res.status < 300 && res.clone) {
              res.clone().text().then(function (t) { scanBody((res && res.url) || target, t); }, function () {});
            }
          } catch (e) {}
        }, function () {});
      } catch (e) {}
      return p;
    };
  }

  var nativeOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    try {
      this.__wbM = method;
      this.__wbU = url;
      this.__wbH = [];
    } catch (e) {}
    return nativeOpen.apply(this, arguments);
  };
  var nativeSetHeader = XMLHttpRequest.prototype.setRequestHeader;
  if (nativeSetHeader) {
    XMLHttpRequest.prototype.setRequestHeader = function (name, value) {
      try {
        readHeader(name, value, this.__wbU);
        if (this.__wbH) collectAuthNames([[name, value]], this.__wbH);
      } catch (e) {}
      return nativeSetHeader.apply(this, arguments);
    };
  }
  var nativeSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.send = function () {
    var xhr = this;
    try {
      xhr.addEventListener("loadend", function () {
        noteCall(xhr.__wbM || "GET", xhr.responseURL || xhr.__wbU, xhr.status, xhr.__wbH || []);
        try {
          if (xhr.status >= 200 && xhr.status < 300 && (xhr.responseType === "" || xhr.responseType === "text")) {
            scanBody(xhr.responseURL || xhr.__wbU, String(xhr.responseText || "").slice(0, 20000));
          }
        } catch (e) {}
      });
    } catch (e) {}
    return nativeSend.apply(this, arguments);
  };

  // Path 2: 存储区。控制台把访问令牌放在 **sessionStorage**（`growth-center-token`，
  // 实测自 www.codebuddy.cn 自己的 JS：它从回跳 URL 的 ?token= 里取出来写进去），
  // 只扫 localStorage 会一无所获。两遍扫：先找形似 JWT 的值，再按键名猜。
  function scanArea(store, area) {
    if (!store || sent) return false;
    var jwt = null;
    for (var i = 0; i < store.length; i++) {
      var k = store.key(i);
      if (!k) continue;
      var v;
      try { v = store.getItem(k); } catch (e) { continue; }
      if (!v) continue;
      if (isJwt(v)) { jwt = { k: k, v: v }; break; }
      if (/(token|jwt|ticket|auth)/i.test(k) && looksSecret(v) && !sent) {
        submit("token", v, "storage:" + area + ":" + k);
        return true;
      }
    }
    if (jwt && !sent) {
      submit("token", jwt.v, "storage:" + area + ":" + jwt.k);
      return true;
    }
    return false;
  }
  function scanStorage() {
    if (sent) return;
    var areas;
    try { areas = [window.localStorage, window.sessionStorage]; } catch (e) { return; }
    for (var i = 0; i < areas.length; i++) {
      if (scanArea(areas[i], i === 0 ? "local" : "session")) return;
    }
  }

  // Path 2.5: 回跳 URL 上的 ?token=。控制台读完会立刻把它从地址栏抹掉（实测：
  // sessionStorage.setItem(M, searchParams.get("token")) 之后 history.replaceState），
  // 所以必须在页面加载的第一时间抓，而且趁它还在时多轮询几次。
  function scanUrlToken() {
    if (sent) return;
    var token = null;
    try {
      var q = new URLSearchParams(window.location.search);
      token = q.get("token") || q.get("access_token");
    } catch (e) { return; }
    if (token && token.length >= 12) submit("token", token, "url:token");
  }

  // Path 3: the cookie header the browser would send. Only works for the
  // non-httpOnly subset, which is why it is last and length-gated.
  function scanCookies() {
    if (sent) return;
    var raw = document.cookie || "";
    if (raw.length < 24) return;
    var pairs = raw.split(";");
    for (var i = 0; i < pairs.length; i++) {
      var kv = pairs[i].trim();
      if (/^(token|jwt|access[_-]?token|session[_-]?id|sid)=/i.test(kv)) {
        submit("cookie", raw, "cookie");
        return;
      }
    }
  }

  // ---- 登录后的引导与页内会话校验 ----
  // web 端的会话是 httpOnly Cookie，页面脚本读不到值，但页面自己 fetch 一次
  // 就知道这套会话在浏览器里到底通不通。结果写进 stats，由主窗轮询决定要不要
  // 把 Cookie 库里的会话当作凭证递给人——没验证过就不再自动塞 Cookie。
  function looksLoggedIn() {
    return !/\/(login|auth)\//i.test(window.location.href);
  }
  function flag(name) {
    try { return window.sessionStorage.getItem(name) === "1"; } catch (e) { return false; }
  }
  function setFlag(name) {
    try { window.sessionStorage.setItem(name, "1"); } catch (e) {}
  }
  function injectBanner() {
    try {
      if (document.getElementById("__wbNote")) return;
      var d = document.createElement("div");
      d.id = "__wbNote";
      d.style.cssText = "position:fixed;right:12px;bottom:12px;z-index:2147483647;max-width:320px;padding:10px 12px;background:#1f2937;color:#f9fafb;font:12px/1.6 system-ui,sans-serif;border-radius:8px;box-shadow:0 4px 16px rgba(0,0,0,.35)";
      d.textContent = "AI Workbench 捕获助手：登录已成功。如需 API Key 凭证，请在「API 管理」页创建一个 Key——密钥只显示一次，创建后会被自动捕获填回主窗。";
      var attach = function () { if (document.body && !document.getElementById("__wbNote")) document.body.appendChild(d); };
      if (document.body) attach();
      else document.addEventListener("DOMContentLoaded", attach);
    } catch (e) {}
  }
  var verifying = false;
  function runVerify() {
    if (!VERIFY_URL || typeof VERIFY_URL !== "string" || flag("__wbVerified") || verifying) return;
    verifying = true;
    var extra = {};
    try {
      var eid = window.sessionStorage.getItem("profile-enterpriseId");
      if (eid) extra["X-Enterprise-Id"] = eid;
    } catch (e) {}
    fetch(VERIFY_URL, { method: "GET", credentials: "include", headers: extra }).then(
      function (r) {
        stats.cookieProbe = Number(r && r.status) | 0;
        stats.cookieOk = stats.cookieProbe >= 200 && stats.cookieProbe < 300;
        verifying = false;
        setFlag("__wbVerified");
      },
      function () {
        stats.cookieProbe = 0;
        verifying = false;
        setFlag("__wbVerified");
      }
    );
  }
  // 扫码落在 /profile/plan，而 API Key 在 /keys 页——跳过去一次，人就能顺手
  // 创建 Key；路由不存在时 SPA 会兜底回首页，无害。
  function bootstrap() {
    if (!looksLoggedIn()) return;
    injectBanner();
    if (!flag("__wbNav") && !/\/keys\b|\/console\//.test(window.location.pathname)) {
      setFlag("__wbNav");
      stats.navigated = 1;
      try { window.location.href = "/keys"; return; } catch (e) {}
    }
    runVerify();
  }

  var ticks = 0;
  scanUrlToken();
  var timer = setInterval(function () {
    ticks += 1;
    stats.ticks = ticks;
    // 前 20 拍（约 10 秒）盯紧 URL：回跳落地后令牌通常几秒内就被脚本抹掉。
    if (ticks <= 20) scanUrlToken();
    scanStorage();
    if (ticks % 5 === 0) scanCookies();
    if (ticks % 2 === 0) bootstrap();
    // Two minutes of a human scanning and typing, then stop poking at storage.
    if (sent || ticks > 240) clearInterval(timer);
  }, 500);
})();
"#;

/// Instantiate the capture script, embedding the adapter's probe URL (if any)
/// as the in-page session-verification target. Escaping goes through
/// `serde_json`, never string concatenation — the URL comes from user-edited
/// protocol JSON.
fn capture_script_with(verify_url: Option<&str>) -> String {
    let target = match verify_url {
        Some(u) => serde_json::to_string(u).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    CAPTURE_SCRIPT.replace("__WB_VERIFY_URL__", &target)
}

/// The probe URL embedded into the script must be a plain http(s) address.
fn verify_url_from(profile: Option<&crate::codebuddy::UpstreamProfile>) -> Option<String> {
    let url = profile?.credential_probe.as_ref()?.url.clone();
    let parsed = Url::parse(&url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(url)
}

fn window_label(batch_id: &str) -> String {
    // Labels cannot contain most punctuation; the batch id is generated by the
    // frontend as a hex string, and we hard-filter anyway so a crafted value
    // cannot address some other window.
    let mut safe: String = batch_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    if safe.is_empty() {
        safe = "default".to_string();
    }
    format!("{WINDOW_PREFIX}{safe}")
}

/// Open the CodeBuddy login page in its own window and start listening for the
/// capture bounce.
///
/// `async` is load-bearing: building a webview from a synchronous command
/// deadlocks the message loop on Windows.
#[tauri::command]
pub async fn wb_open_cb_login_window(app: AppHandle, batch_id: String) -> Result<String, String> {
    let (login_url, script) = {
        let state = app.state::<crate::DbState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        let profile = crate::wb_commands::get_settings(&conn)?
            .adapter_json
            .filter(|json| !json.trim().is_empty())
            .and_then(|json| crate::codebuddy::UpstreamProfile::from_json(&json).ok());
        let url = profile
            .as_ref()
            .map(|p| p.login_url.clone())
            .unwrap_or_default();
        (url, capture_script_with(verify_url_from(profile.as_ref()).as_deref()))
    };
    let login_url = login_url.trim().to_string();
    if login_url.is_empty() {
        return Err("协议里还没有配置 loginUrl，无法打开登录窗（可先用「粘贴凭证纳管」）".to_string());
    }
    let parsed = Url::parse(&login_url).map_err(|e| format!("loginUrl 非法: {e}"))?;

    let label = window_label(&batch_id);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        let _ = existing.eval(&script);
        return Ok(login_url);
    }

    let app_for_nav = app.clone();
    let batch_for_nav = batch_id.clone();
    let label_for_close = label.clone();

    let window = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title("CodeBuddy 登录 / 扫码")
        .inner_size(980.0, 760.0)
        .min_inner_size(420.0, 480.0)
        .resizable(true)
        .decorations(true)
        .center()
        .focused(true)
        .initialization_script(&script)
        .on_navigation(move |url: &Url| {
            if url.host_str() != Some(CAPTURE_HOST) {
                return true;
            }
            let mut kind = "token".to_string();
            let mut via = String::new();
            let mut value = String::new();
            let mut truncated = false;
            for (key, val) in url.query_pairs() {
                match key.as_ref() {
                    "kind" => kind = val.to_string(),
                    "via" => via = val.to_string(),
                    "v" => value = val.to_string(),
                    "truncated" => truncated = val == "1",
                    _ => {}
                }
            }
            // The bounce itself must not be navigated to, and the script latches
            // after one hit, so this fires at most once per window.
            let payload = CapturedCredential {
                batch_id: batch_for_nav.clone(),
                credential_type: kind,
                truncated: truncated || value.chars().count() >= MAX_VALUE_CHARS,
                credential_raw: value,
                via,
            };
            let handle = app_for_nav.clone();
            let label = label_for_close.clone();
            tauri::async_runtime::spawn(async move {
                if payload.credential_raw.is_empty() || payload.truncated {
                    // Nothing usable: leave the window open so the user can
                    // still copy something by hand, and let the UI fall back.
                } else if let Some(win) = handle.get_webview_window(&label) {
                    let _ = win.close();
                }
                let _ = handle.emit(EVENT_CAPTURED, &payload);
            });
            false
        })
        .build()
        .map_err(|e| format!("创建登录窗口失败: {e}"))?;

    let _ = window.set_focus();
    Ok(login_url)
}

/// Close a still-open login window (the UI's Cancel button).
#[tauri::command]
pub async fn wb_close_cb_login_window(app: AppHandle, batch_id: String) -> Result<(), String> {
    let label = window_label(&batch_id);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CLOSE_DELAY).await;
        if let Some(win) = handle.get_webview_window(&label) {
            let _ = win.close();
        }
    });
    Ok(())
}

/// Cookie-store capture. The console authenticates with httpOnly session
/// cookies, which neither `document.cookie` nor the request-header hook can
/// see, so the in-page script comes back empty even after a successful login.
/// The runtime's cookie jar does contain them.
///
/// Windows caveat: `cookies()` posts a message to the event loop and blocks on
/// the reply, so it must never run on the main thread — same failure class as
/// building a window from a synchronous command.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieCapture {
    pub page_url: String,
    /// False while the window still sits on the login page: cookies there are
    /// CSRF/state noise, not a session, so the UI must not treat them as one.
    pub logged_in: bool,
    pub cookie_header: String,
    pub names: Vec<String>,
    pub truncated: bool,
}

/// Standard cookie scoping: a cookie set for `.codebuddy.cn` applies to
/// `www.codebuddy.cn`, but a `.tencent.com` SSO cookie must not leak into a
/// header we send to codebuddy.
fn domain_matches(host: &str, cookie_domain: &str) -> bool {
    let d = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let h = host.to_ascii_lowercase();
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

fn scoped_cookies(host: &str, cookies: &[(String, String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value, domain) in cookies {
        if name.is_empty() || value.is_empty() || !domain_matches(host, domain) {
            continue;
        }
        // A later, more specific cookie of the same name wins (RFC 6265 order).
        if let Some(existing) = out.iter_mut().find(|(n, _)| n == name) {
            existing.1 = value.clone();
        } else {
            out.push((name.clone(), value.clone()));
        }
    }
    out
}

fn join_cookie(scoped: &[(String, String)]) -> String {
    scoped
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The login SPA keeps `/login` in the path until it redirects away, so a URL
/// still under it means those cookies are CSRF/state noise, not a session.
fn logged_in_from_url(url: &str) -> bool {
    !url.to_ascii_lowercase().contains("/login")
}

/// Read the login window's cookies and hand back a pasteable `Cookie:` value.
#[tauri::command]
pub async fn wb_capture_cb_login_cookies(app: AppHandle, batch_id: String) -> Result<CookieCapture, String> {
    let label = window_label(&batch_id);
    let handle = app.clone();
    let (page_url, host, cookies) = tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<(String, String, String)>), String> {
        let win = handle
            .get_webview_window(&label)
            .ok_or_else(|| "登录窗已关闭，请重新发起扫码".to_string())?;
        let url = win.url().map_err(|e| e.to_string())?;
        let host = url.host_str().unwrap_or_default().to_string();
        let cookies = win
            .cookies()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|c| {
                (
                    c.name().to_string(),
                    c.value().to_string(),
                    c.domain().unwrap_or_default().to_string(),
                )
            })
            .collect();
        Ok((url.to_string(), host, cookies))
    })
    .await
    .map_err(|e| e.to_string())??;

    let scoped = scoped_cookies(&host, &cookies);
    let joined = join_cookie(&scoped);
    let truncated = joined.chars().count() > MAX_VALUE_CHARS;
    let cookie_header = if truncated {
        joined.chars().take(MAX_VALUE_CHARS).collect()
    } else {
        joined
    };
    Ok(CookieCapture {
        logged_in: logged_in_from_url(&page_url),
        names: scoped.into_iter().map(|(name, _)| name).collect(),
        cookie_header,
        truncated,
        page_url,
    })
}

/// 自检回包上限：只递状态码和一小段响应头，凭证/正文不外传。
const SELFTEST_BODY_CHARS: usize = 160;
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 探针记录的一次调用：方法 / URL / 状态码 / 哪些请求头像凭证（只留名字）/ 出现次数。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeCall {
    pub m: String,
    pub u: String,
    #[serde(default)]
    pub s: u16,
    #[serde(default)]
    pub a: String,
    #[serde(default)]
    pub n: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub page_url: String,
    pub calls: Vec<ProbeCall>,
    /// 窗口刚开、或脚本没注进去（新标签页/CSP）时为 true。
    pub empty: bool,
    /// 注入脚本的自诊断计数（`__wbCaptureStats`）：脚本是否活着、观察到的请求/认证头、
    /// 页内会话校验结果。None 表示读不到 —— 脚本没跑起来。
    pub capture_stats: Option<serde_json::Value>,
}

const PROBE_READ_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_PROBE_CALLS: usize = 60;

/// `eval_with_callback` 的回执 → 报告。WebView2 会吞掉 JS 异常，所以坏 JSON/null 只能
/// 降级成空报告，不能让命令整体失败（否则用户看不到"窗口还开着"这个事实）。
fn parse_probe_report(raw: &str) -> ProbeReport {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        page: String,
        #[serde(default)]
        list: Vec<ProbeCall>,
        #[serde(default)]
        stats: Option<serde_json::Value>,
    }
    let wire: Wire = serde_json::from_str(raw).unwrap_or(Wire {
        page: String::new(),
        list: Vec::new(),
        stats: None,
    });
    let mut calls = wire.list;
    if calls.len() > MAX_PROBE_CALLS {
        calls = calls.split_off(calls.len() - MAX_PROBE_CALLS);
    }
    ProbeReport {
        page_url: wire.page,
        empty: calls.is_empty(),
        calls,
        capture_stats: wire.stats,
    }
}

/// 读取登录窗里探针攒下的接口清单：不导航、不打断登录页，直接取 JS 求值结果。
#[tauri::command]
pub async fn wb_read_cb_login_probe(app: AppHandle, batch_id: String) -> Result<ProbeReport, String> {
    let label = window_label(&batch_id);
    let handle = app.clone();
    let raw = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let win = handle
            .get_webview_window(&label)
            .ok_or_else(|| "登录窗已关闭，请重新发起扫码".to_string())?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        // eval_with_callback 不阻塞，阻塞的是等回执 —— 所以整段都必须在非主线程上跑。
        win.eval_with_callback(
            "(function(){try{return {page:(window.__wbProbe&&window.__wbProbe.page)||location.href,list:(window.__wbProbe&&window.__wbProbe.list)||[],stats:window.__wbCaptureStats||null};}catch(e){return {page:'',list:[],stats:null};}})()",
            move |value| {
                let _ = tx.send(value);
            },
        )
        .map_err(|e| format!("注入失败: {e}"))?;
        rx.recv_timeout(PROBE_READ_TIMEOUT)
            .map_err(|_| "读取超时：登录窗没有响应探针脚本（可能页面已跳走）".to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(parse_probe_report(&raw))
}

/// 自检结果。`status == 0` 表示没跑成（窗口没响应 / 超时 / fetch 抛错）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTestOutcome {
    pub url: String,
    pub status: u16,
    pub body_head: String,
    pub error: Option<String>,
}

/// 生成"在页面里 fetch 一次"的脚本。URL/方法都走 `serde_json` 转义，拼字符串
/// 不能省 —— 探针清单里的 URL 是页面给的，带引号就能把脚本戳穿。
fn selftest_script(method: &str, url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|_| "只能试调完整的 http(s) 地址".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("只能试调 http(s) 地址".to_string());
    }
    // fetch 的 method 是大小写敏感的 token，页面给的却是小写。
    let m: String = if method.trim().is_empty() {
        "GET".to_string()
    } else {
        method.trim().to_uppercase()
    };
    let jm = serde_json::to_string(&m).map_err(|_| "方法名不合法".to_string())?;
    let ju = serde_json::to_string(url).map_err(|e| e.to_string())?;
    Ok(format!(
        "(function(){{var cap={cap};window.__wbSelfTest=null;fetch({ju},{{method:{jm},credentials:'include'}})\
         .then(function(r){{return r.text().then(function(t){{window.__wbSelfTest=\
           {{url:{ju},status:r.status,body:(t||'').slice(0,cap)}};}});}},\
         function(e){{window.__wbSelfTest={{url:{ju},status:0,body:'',error:String(e&&e.message||e)}};}});}})()",
        cap = SELFTEST_BODY_CHARS,
        ju = ju,
        jm = jm,
    ))
}

fn pending_result(url: &str) -> String {
    let ju = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".into());
    format!("{{\"url\":{ju},\"status\":0,\"body\":\"\",\"error\":\"timeout\"}}")
}

fn parse_selftest(raw: &str) -> SelfTestOutcome {
    #[derive(serde::Deserialize, Default)]
    struct Wire {
        #[serde(default)]
        url: String,
        #[serde(default)]
        status: u16,
        #[serde(default)]
        body: String,
        #[serde(default)]
        error: Option<String>,
    }
    let wire: Wire = serde_json::from_str(raw).unwrap_or_default();
    SelfTestOutcome {
        url: wire.url,
        status: wire.status,
        body_head: wire.body.chars().take(SELFTEST_BODY_CHARS).collect(),
        error: wire.error,
    }
}

/// 在登录窗里"就地"试调一个端点，用来回答一个服务端探测永远答不了的问题：
/// 这个端点在**浏览器自己的会话**下到底通不通。
///
/// 凭证能不能搬出浏览器，只有浏览器自己知道：控制台那些 `/console/api/*` 路由
/// 用我们抓到的 Cookie 重放，返回和"完全不带凭证"一字不差的 401（实测），说明
/// 会话根本不可重放。这个自检把 fetch 丢回页面里跑，状态码 + 一小段响应正文原样
/// 递回来，人一眼就能判"端点错了"还是"凭证带不出去"。
#[tauri::command]
pub async fn wb_cb_login_selftest(
    app: AppHandle,
    batch_id: String,
    method: String,
    url: String,
) -> Result<SelfTestOutcome, String> {
    let label = window_label(&batch_id);
    let js = selftest_script(&method, &url)?;
    let handle = app.clone();
    let raw = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let win = handle
            .get_webview_window(&label)
            .ok_or_else(|| "登录窗已关闭，请重新发起扫码".to_string())?;
        // 注入脚本自己会把 __wbSelfTest 复位成 null，所以不必（也不能）在这里再 eval 一次。
        win.eval(&js).map_err(|e| format!("注入失败: {e}"))?;
        let pending = "null";
        let started = std::time::Instant::now();
        loop {
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            win.eval_with_callback(
                "(function(){try{return window.__wbSelfTest===undefined?null:window.__wbSelfTest;}catch(e){return null;}})()",
                move |value| {
                    let _ = tx.send(value);
                },
            )
            .map_err(|e| format!("读取失败: {e}"))?;
            let got = rx
                .recv_timeout(PROBE_READ_TIMEOUT)
                .map_err(|_| "登录窗没有响应（可能页面已跳走）".to_string())?;
            if got != pending && !got.is_empty() {
                return Ok(got);
            }
            if started.elapsed() > SELFTEST_TIMEOUT {
                return Ok(pending_result(&url));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(parse_selftest(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_labels_are_sanitized_and_prefixed() {
        assert_eq!(window_label("ab12"), "wb-cb-login-ab12");
        // Path traversal / label collisions must not survive.
        assert_eq!(window_label("../../main"), "wb-cb-login-main");
        assert_eq!(window_label("a-b c!"), "wb-cb-login-abc");
        assert_eq!(window_label(""), "wb-cb-login-default");
        assert_eq!(window_label(&"x".repeat(200)).len(), WINDOW_PREFIX.len() + 32);
    }

    #[test]
    fn capture_script_keeps_the_host_and_latches_after_one_hit() {
        assert!(CAPTURE_SCRIPT.contains(CAPTURE_HOST));
        assert!(CAPTURE_SCRIPT.contains("__wbCaptureInstalled"));
        // Both capture paths and the fallback must be present.
        assert!(CAPTURE_SCRIPT.contains("setRequestHeader"));
        assert!(CAPTURE_SCRIPT.contains("localStorage"));
        assert!(CAPTURE_SCRIPT.contains("document.cookie"));
        assert!(CAPTURE_SCRIPT.contains("clearInterval"));
    }

    #[test]
    fn payload_ceiling_matches_the_scripts_own_clip() {
        // The JS clips to 24000 and flags it; Rust must agree or a truncated
        // credential would be accepted silently.
        assert_eq!(MAX_VALUE_CHARS, 24_000);
        assert!(CAPTURE_SCRIPT.contains("24000"));
    }

    #[test]
    fn cookie_scoping_keeps_only_the_page_domain() {
        let cookies = vec![
            ("sid".to_string(), "a".to_string(), ".codebuddy.cn".to_string()),
            ("host_only".to_string(), "b".to_string(), "www.codebuddy.cn".to_string()),
            ("sso".to_string(), "c".to_string(), ".tencent.com".to_string()),
            ("blank".to_string(), String::new(), ".codebuddy.cn".to_string()),
        ];
        let scoped = scoped_cookies("www.codebuddy.cn", &cookies);
        let names: Vec<&str> = scoped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["sid", "host_only"]);
        assert_eq!(join_cookie(&scoped), "sid=a; host_only=b");
    }

    #[test]
    fn duplicate_cookie_names_collapse_to_the_more_specific_value() {
        let cookies = vec![
            ("t".to_string(), "wildcard".to_string(), ".codebuddy.cn".to_string()),
            ("t".to_string(), "exact".to_string(), "www.codebuddy.cn".to_string()),
        ];
        assert_eq!(join_cookie(&scoped_cookies("www.codebuddy.cn", &cookies)), "t=exact");
    }

    #[test]
    fn cookie_domains_never_match_across_boundaries() {
        assert!(domain_matches("www.codebuddy.cn", "codebuddy.cn"));
        assert!(domain_matches("codebuddy.cn", ".codebuddy.cn"));
        assert!(!domain_matches("evilcodebuddy.cn", "codebuddy.cn"));
        assert!(!domain_matches("a.b.example.com", "example.com.evil.cn"));
        assert!(!domain_matches("www.codebuddy.cn", ""));
    }

    #[test]
    fn login_pages_are_not_reported_as_logged_in() {
        assert!(!logged_in_from_url("https://www.codebuddy.cn/login/?platform=usercenter"));
        assert!(!logged_in_from_url("https://WWW.CODEBUDDY.CN/Login/"));
        assert!(logged_in_from_url("https://www.codebuddy.cn/profile/plan"));
    }

    #[test]
    fn probe_report_reads_the_page_own_calls() {
        let raw = r#"{"page":"https://www.codebuddy.cn/profile/plan","list":[
            {"m":"GET","u":"https://www.codebuddy.cn/console/api/client/v1/api-keys","s":200,"a":"","n":3},
            {"m":"POST","u":"https://www.codebuddy.cn/v2/chat","s":401,"a":"authorization","n":1}
        ]}"#;
        let report = parse_probe_report(raw);
        assert!(!report.empty);
        assert_eq!(report.calls.len(), 2);
        assert_eq!(report.calls[0].n, 3);
        assert_eq!(report.calls[1].a, "authorization");
        assert!(report.page_url.ends_with("/profile/plan"));
    }

    #[test]
    fn a_broken_probe_eval_degrades_to_an_empty_report() {
        // WebView2 吞异常时会给出 null / 空串 / 半截 JSON，都不能让命令炸掉。
        for raw in ["null", "", "{", "not json"] {
            let report = parse_probe_report(raw);
            assert!(report.empty, "{raw:?} should be empty");
            assert!(report.calls.is_empty());
        }
    }

    #[test]
    fn probe_reports_are_capped_at_the_newest_calls() {
        let list: Vec<String> = (0..90)
            .map(|i| format!(r#"{{"m":"GET","u":"https://x/api/{i}","s":200,"a":"","n":1}}"#))
            .collect();
        let report = parse_probe_report(&format!(r#"{{"page":"p","list":[{}]}}"#, list.join(",")));
        assert_eq!(report.calls.len(), MAX_PROBE_CALLS);
        // 留的是最新的尾巴，不是最早 60 条。
        assert!(report.calls.last().unwrap().u.ends_with("/89"));
        assert!(report.calls.first().unwrap().u.ends_with("/30"));
    }

    #[test]
    fn capture_script_installs_the_probe_recorder() {
        assert!(CAPTURE_SCRIPT.contains("__wbProbe"));
        assert!(CAPTURE_SCRIPT.contains("noteCall"));
        assert!(CAPTURE_SCRIPT.contains("responseURL"));
        // 探针只留头名，绝不把头的值带出去。
        assert!(!CAPTURE_SCRIPT.contains("probe.list.push({ m: method, value"));
        // 原有的凭证抓取不能被探针挤掉。
        assert!(CAPTURE_SCRIPT.contains("readHeaderSet"));
        assert!(CAPTURE_SCRIPT.contains("readHeader(name, value, url)"));
    }

    #[test]
    fn capture_script_is_one_iife_and_ends_with_it() {
        // 注入脚本没有 IPC 桥，语法错了不会有任何提示——Path 2 曾经被一个提前
        // 关闭的 `})();` 甩到函数体外，整段抓取静默失效。结构性检查比肉眼可靠。
        assert!(CAPTURE_SCRIPT.trim_start().starts_with("(function ()"));
        assert_eq!(CAPTURE_SCRIPT.matches("})();").count(), 1);
        assert!(CAPTURE_SCRIPT.trim_end().ends_with("})();"));
        // 三条抓取路径必须都还在同一个函数体里。
        for needle in ["scanUrlToken", "scanStorage", "scanCookies", "setInterval"] {
            assert!(CAPTURE_SCRIPT.contains(needle), "{needle} missing");
        }
    }

    #[test]
    fn capture_script_reads_the_session_storage_and_the_redirect_token() {
        // 实测：控制台把访问令牌放在回跳 URL 的 ?token= 里，读完立刻写进
        // sessionStorage('growth-center-token') 再把参数抹掉。只扫 localStorage
        // 的旧版本两头都抓不到，所以这两条路径必须都在。
        assert!(CAPTURE_SCRIPT.contains("window.sessionStorage"));
        assert!(CAPTURE_SCRIPT.contains("scanUrlToken"));
        assert!(CAPTURE_SCRIPT.contains("\"url:token\""));
        // 令牌几秒内就被脚本抹掉，前 10 秒必须高频盯 URL。
        assert!(CAPTURE_SCRIPT.contains("ticks <= 20"));
    }

    #[test]
    fn capture_script_v2_reports_its_own_liveness() {
        // web 端 usercenter 的 axios 只在 miniProgram 模式才发 Authorization 头
        // （其 config chunk 实测），头部钩子注定落空——自诊断统计是判断"脚本死了
        // 还是页面本来就没东西"的唯一手段。
        assert!(CAPTURE_SCRIPT.contains("__wbCaptureStats"));
        assert!(CAPTURE_SCRIPT.contains("stats.bodyScans"));
        // API Key 只在创建响应里出现一次，必须扫响应体。
        assert!(CAPTURE_SCRIPT.contains("scanBody"));
        assert!(CAPTURE_SCRIPT.contains("responseText"));
        // 页内会话校验：带 cookie 的 fetch + 结果落 stats。
        assert!(CAPTURE_SCRIPT.contains("credentials: \"include\""));
        assert!(CAPTURE_SCRIPT.contains("cookieOk"));
        // 扫码落在 /profile/plan，API Key 在 /keys —— 引导跳转只能发生一次。
        assert!(CAPTURE_SCRIPT.contains("/keys"));
        assert!(CAPTURE_SCRIPT.contains("__wbNav"));
        // 凭证捕获必须限定 CodeBuddy 自家域：登录页里 QQ 票据 iframe 的响应
        // 带 "token" 字段，实测它第一个被 latch（browsertdidticket.m.qq.com）。
        assert!(CAPTURE_SCRIPT.contains("codebuddy\\.cn|copilot\\.tencent\\.com|lkeap\\.cloud\\.tencent\\.com"));
        assert!(CAPTURE_SCRIPT.contains("trustedUrl"));
    }

    #[test]
    fn capture_script_embeds_the_verify_url_safely() {
        let plain = capture_script_with(None);
        assert!(!plain.contains("__WB_VERIFY_URL__"));
        assert!(plain.contains("VERIFY_URL = null"));
        assert_eq!(plain.matches("})();").count(), 1);

        let evil = "https://x.test/a?b=\"quoted\"&c=</script>";
        let js = capture_script_with(Some(evil));
        assert!(!js.contains("__WB_VERIFY_URL__"));
        assert!(js.contains("\\\"quoted\\\""), "{js}");
        assert!(!js.contains("\"</script>\""));
        // 注入后仍是一段语法完整的单 IIFE。
        assert_eq!(js.matches("})();").count(), 1);
    }

    #[test]
    fn verify_url_only_accepts_http_targets_from_the_profile() {
        let profile: crate::codebuddy::UpstreamProfile =
            serde_json::from_str(r#"{"loginUrl":"https://x.test/login","credentialProbe":{"method":"GET","url":"https://x.test/probe"}}"#).unwrap();
        assert_eq!(verify_url_from(Some(&profile)).as_deref(), Some("https://x.test/probe"));

        let javascript: crate::codebuddy::UpstreamProfile =
            serde_json::from_str(r#"{"loginUrl":"https://x.test/login","credentialProbe":{"method":"GET","url":"javascript:alert(1)"}}"#).unwrap();
        assert!(verify_url_from(Some(&javascript)).is_none());
        assert!(verify_url_from(None).is_none());
    }

    #[test]
    fn probe_report_carries_the_capture_stats() {
        let raw = r#"{"page":"https://www.codebuddy.cn/keys","list":[],
            "stats":{"installed":1,"ticks":9,"calls":7,"auth":0,"bodyScans":3,"tokens":0,"cookieProbe":200,"cookieOk":true,"navigated":1}}"#;
        let report = parse_probe_report(raw);
        let stats = report.capture_stats.expect("stats should survive");
        assert_eq!(stats["cookieProbe"], 200);
        assert_eq!(stats["cookieOk"], true);
        // 旧脚本/读不到脚本时 stats 为 null，不能炸。
        assert!(parse_probe_report(r#"{"page":"p","list":[]}"#).capture_stats.is_none());
        assert!(parse_probe_report("null").capture_stats.is_none());
    }

    #[test]
    fn selftest_script_only_addresses_http_and_escapes_the_url() {
        assert!(selftest_script("GET", "javascript:alert(1)").is_err());
        assert!(selftest_script("GET", "not a url").is_err());
        assert!(selftest_script("GET", "file:///c:/windows/win.ini").is_err());
        let js = selftest_script("post", "https://x.test/a?b=\"quoted\"&c=</script>").unwrap();
        // 引号与 </ 必须被 JSON 转义掉，否则一个 URL 就能戳穿我们注入的脚本。
        assert!(js.contains("\\\"quoted\\\""), "{js}");
        assert!(!js.contains("\"</script>\""));
        assert!(js.contains("\"POST\""));
        assert!(js.contains("credentials:'include'"));
    }

    #[test]
    fn selftest_replies_degrade_to_a_status_zero_readout() {
        let ok = parse_selftest("{\"url\":\"https://x/a\",\"status\":200,\"body\":\"{}\"}");
        assert_eq!(ok.status, 200);
        assert_eq!(ok.error, None);
        // WebView2 吞异常时会回 null / 半截 JSON，不能 panic。
        assert_eq!(parse_selftest("null").status, 0);
        assert_eq!(parse_selftest("{oops").status, 0);
        let timeout: serde_json::Value =
            serde_json::from_str(&pending_result("https://x/a")).unwrap();
        assert_eq!(timeout["status"], 0);
        assert_eq!(timeout["url"], "https://x/a");
    }
}
