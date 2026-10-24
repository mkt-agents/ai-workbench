use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{Manager, Theme, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_shell::ShellExt;
use url::Url;

/// Magic hostname used to trigger "open in system browser" from inside the
/// injected toolbar. The toolbar sets an `<a>` href to
/// `https://aiwb-shell.open/?url=<encoded target>` and clicks it; the
/// `on_navigation` handler on the webview intercepts this host, calls
/// `shell.open(target)`, and returns false to stop the webview from actually
/// loading the placeholder URL. This works on external-URL webviews where
/// the Tauri IPC bridge is not injected (so `__TAURI_INTERNALS__.invoke`
/// is unavailable).
const SHELL_OPEN_TRIGGER_HOST: &str = "aiwb-shell.open";

/// Second magic host: "问天玑" in the injected toolbar navigates to
/// `https://aiwb-aihub.open/?text=<encoded selection>` and we forward the
/// text to the ai-hub window via `open_ai_hub_with_text`. Same trick as
/// `SHELL_OPEN_TRIGGER_HOST`: external-URL webviews have no IPC bridge.
const AI_HUB_TRIGGER_HOST: &str = "aiwb-aihub.open";

/// Third magic host: collapse/expand clicks in the injected toolbar report
/// the new state via `https://aiwb-toolbar.open/?collapsed=1|0`. The Rust
/// side remembers it globally, so every future browser window (any site,
/// any app restart) opens with the user's last choice. First-ever use has
/// no record yet and defaults to collapsed.
const TOOLBAR_STATE_TRIGGER_HOST: &str = "aiwb-toolbar.open";

/// Marker file in the app-data dir holding the last collapsed state ("1"/"0").
const TOOLBAR_STATE_FILE: &str = "browser-toolbar-collapsed";

/// Global toolbar collapsed state, loaded from disk on first use.
static TOOLBAR_COLLAPSED: OnceLock<AtomicBool> = OnceLock::new();

fn toolbar_collapsed_state(app: &tauri::AppHandle) -> &AtomicBool {
    TOOLBAR_COLLAPSED.get_or_init(|| {
        let persisted = app
            .path()
            .app_data_dir()
            .ok()
            .and_then(|dir| std::fs::read_to_string(dir.join(TOOLBAR_STATE_FILE)).ok())
            .map(|s| s.trim() == "1")
            // No record yet → first-ever open starts collapsed.
            .unwrap_or(true);
        AtomicBool::new(persisted)
    })
}

fn persist_toolbar_collapsed(app: &tauri::AppHandle, collapsed: bool) {
    if let Ok(dir) = app.path().app_data_dir() {
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(
                dir.join(TOOLBAR_STATE_FILE),
                if collapsed { "1" } else { "0" },
            );
        }
    }
}

fn is_safe_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[derive(Deserialize)]
pub struct UserscriptInit {
    pub name: String,
    pub code: String,
    /// The script's `@match` patterns. The window's init script re-runs on every
    /// navigation, so each script has to re-check them against the *current* URL —
    /// otherwise a script bound to one site keeps executing on whatever the user
    /// clicks through to. Missing/empty means "run everywhere", i.e. the old behaviour,
    /// rather than failing the whole window creation.
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// Navigate existing "browser" webview window. Returns true if navigated, false if window missing.
#[tauri::command]
pub fn navigate_browser_window(app: tauri::AppHandle, url: String) -> Result<bool, String> {
    if !is_safe_url(&url) {
        return Err("仅允许 http/https 地址".to_string());
    }
    let parsed: Url = url
        .parse()
        .map_err(|e| format!("无效 URL: {}", e))?;
    match app.get_webview_window("browser") {
        Some(w) => {
            w.navigate(parsed).map_err(|e| format!("导航失败: {}", e))?;
            let _ = w.set_focus();
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Floating toolbar injected into every opened browser window. Runs on every
/// page load via `WebviewWindowBuilder::initialization_script` (top-level
/// frame only — see the guard at the top of the script), so it survives
/// in-page navigation. Uses Shadow DOM to avoid colliding with the host page's
/// styles. Toolbar features:
///   - Back / Forward / Reload / Home
///   - Zoom out / display (click to reset) / zoom in / reset — via
///     body.style.zoom (WebView2 + WKWebView both support it)
///   - URL chip (click to copy) / Copy / Open in external browser
///   - Drag the whole bar (not just the grip); double-click re-docks to the
///     bottom-right corner. A drag mask keeps pointermove alive over
///     cross-origin iframes while dragging.
///   - Collapse / expand with an animated width transition
///   - Hide, with a small restore dot in the corner to bring it back
///   - Position / zoom persist per-origin via sessionStorage, so they
///     survive in-page navigation in this window. The collapsed state is
///     remembered globally: clicks are reported back to the shell via the
///     `aiwb-toolbar.open` magic host and persisted on the Rust side, so
///     the next window on any site opens with the last choice (collapsed
///     on first-ever use).
///
/// The toolbar calls the webview's own `history.back()/forward()` and
/// `location.reload()` — no IPC round-trip needed for nav actions. Opening
/// the external browser navigates to a magic placeholder hostname
/// (`aiwb-shell.open`) that the Rust-side `on_navigation` handler intercepts
/// and forwards to `shell.open`; the URL is also copied as a fallback.
const BROWSER_TOOLBAR_INIT_JS: &str = r#"(function () {
  // ---- Popup / OAuth / target=_blank support (runs in EVERY frame) ----
  // The webview has no on_new_window handler, so native window.open() calls
  // and target="_blank" anchors are silently dropped — login buttons (Cursor,
  // GitHub, Google…) would do nothing. Normalize both into same-window
  // navigations so the OAuth redirect flow completes inside this webview.
  // This must run in iframes too: login widgets often live in one, and a
  // cross-origin frame is allowed to navigate the top window.
  if (!window.__aiWorkbenchPopupPatched) {
    var __origOpen = window.open;
    var __navigateSameTab = function (url) {
      // In an iframe, send the navigation to the top window: an OAuth chain
      // that lands back on the site then logs the whole page in. Navigating
      // top from a cross-origin frame is permitted; same-origin too.
      var w = window.top !== window.self ? window.top : window;
      try {
        w.location.href = url;
      } catch (_) {
        window.location.href = url;
      }
    };
    var __shellRelay = function (url) {
      var a = document.createElement('a');
      a.href = 'https://aiwb-shell.open/?url=' + encodeURIComponent(url);
      a.style.cssText = 'display:none;position:fixed;top:-9999px;left:-9999px;';
      (document.body || document.documentElement).appendChild(a);
      a.click();
      setTimeout(function () { if (a.parentNode) a.parentNode.removeChild(a); }, 200);
    };
    window.open = function (url, target, features) {
      if (url && url.indexOf('aiwb-shell.open') !== -1) {
        // Already encoded for shell.open — let the navigation handler deal
        // with it by dispatching a click on a hidden link.
        __shellRelay(url);
        return { closed: false, focus: function () {}, close: function () {} };
      }
      if (url && /^https?:\/\//.test(url)) {
        // External URL — navigate so the OAuth popup flow becomes a same-tab
        // redirect flow. Returns `window` so callers that expect a WindowProxy
        // (e.g. to call .close() or .postMessage()) don't throw.
        __navigateSameTab(String(url));
        return window;
      }
      // about:blank, javascript:, or no URL — fall back to the original
      // (which will still likely be a no-op, but we don't break edge cases).
      try {
        return __origOpen.apply(window, arguments);
      } catch (_) {
        return { closed: false, focus: function () {}, close: function () {} };
      }
    };
    // target="_blank" anchors are dropped exactly like window.open was.
    document.addEventListener('click', function (e) {
      if (e.defaultPrevented) return;
      var el = e.target;
      while (el && el.tagName !== 'A') {
        el = el.parentElement;
      }
      if (!el) return;
      var href = el.getAttribute('href');
      if (!href || href.indexOf('javascript:') === 0 || href.charAt(0) === '#') return;
      if (String(el.target || '').toLowerCase() !== '_blank') return;
      e.preventDefault();
      try {
        var abs = new URL(href, location.href);
        if (abs.protocol !== 'http:' && abs.protocol !== 'https:') return;
        __navigateSameTab(abs.href);
      } catch (_) {
        /* ignore malformed hrefs */
      }
    }, true);
    Object.defineProperty(window, '__aiWorkbenchPopupPatched', {
      value: true,
      writable: false,
      configurable: false,
    });
  }

  // WebView2 runs initialization scripts in EVERY frame (top-level + all
  // iframes). Pages with embedded iframes (e.g. QR-code login widgets) would
  // get one toolbar per iframe, each fixed to the iframe's own viewport and
  // stacked over the page content. Only the top-level frame gets a toolbar.
  // Comparing window.top/window.self is safe cross-origin (unlike reading
  // top.location), so no try/catch needed.
  if (window.top !== window.self) return;

  // ---- Loading overlay -------------------------------------------------
  // Between window creation and the remote page's first paint the frame is
  // just black. An overlay injected at document-create time hides that; it
  // fades out on `load`, with a hard timeout so a page that never fires load
  // cannot trap the user. Re-armed automatically on every real navigation
  // (this script re-runs), while SPA pushState hops don't re-trigger it.
  if (!document.getElementById('aiwb-loading-overlay')) {
    var __ov = document.createElement('div');
    __ov.id = 'aiwb-loading-overlay';
    __ov.style.cssText =
      'position:fixed;inset:0;z-index:2147483646;display:flex;align-items:center;justify-content:center;background:#12151a;transition:opacity 0.25s ease;';
    __ov.innerHTML =
      '<style>@keyframes aiwb-spin{to{transform:rotate(360deg);}}' +
      '.aiwb-spin{width:30px;height:30px;border-radius:50%;border:2.5px solid rgba(255,255,255,0.15);border-top-color:#e5e7eb;animation:aiwb-spin 0.9s linear infinite;}</style>' +
      '<div class="aiwb-spin"></div>';
    // WebView2 (runtime >= 150) runs document-created scripts BEFORE the HTML
    // parser has created <html>/<body> — document.body and
    // document.documentElement are both null at that moment, so a bare
    // appendChild throws and kills the whole IIFE (toolbar included). Poll
    // until the parser creates the root element, usually the very next tick.
    var __ovAttached = false;
    var __ovAttach = function () {
      if (__ovAttached) return;
      var base = document.body || document.documentElement;
      if (!base) return;
      try {
        base.appendChild(__ov);
      } catch (_) { /* document torn down mid-navigation */ }
      __ovAttached = true;
      clearInterval(__ovTimer);
    };
    var __ovTimer = setInterval(__ovAttach, 16);
    setTimeout(function () { clearInterval(__ovTimer); }, 2000);
    __ovAttach();
    var __ovDone = false;
    var __ovRemove = function () {
      if (__ovDone) return;
      __ovDone = true;
      try {
        __ov.style.opacity = '0';
        setTimeout(function () { if (__ov.parentNode) __ov.parentNode.removeChild(__ov); }, 280);
      } catch (_) { /* the document may already be gone */ }
    };
    if (document.readyState === 'complete') {
      setTimeout(__ovRemove, 150);
    } else {
      window.addEventListener('load', function () { setTimeout(__ovRemove, 150); });
      setTimeout(__ovRemove, 10000);
    }
  }

  if (window.__aiWorkbenchToolbarInjected) return;
  Object.defineProperty(window, '__aiWorkbenchToolbarInjected', {
    value: true,
    writable: false,
    configurable: false,
  });

  // ---- Helpers -----------------------------------------------------------

  // Inline SVG (feather-style) icons: crisp at any DPI and consistent across
  // systems, unlike the unicode glyphs used before.
  function svg(body) {
    return '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + body + '</svg>';
  }
  var ICONS = {
    grip: '<circle cx="9" cy="6" r="1.3" fill="currentColor" stroke="none"/><circle cx="15" cy="6" r="1.3" fill="currentColor" stroke="none"/><circle cx="9" cy="12" r="1.3" fill="currentColor" stroke="none"/><circle cx="15" cy="12" r="1.3" fill="currentColor" stroke="none"/><circle cx="9" cy="18" r="1.3" fill="currentColor" stroke="none"/><circle cx="15" cy="18" r="1.3" fill="currentColor" stroke="none"/>',
    back: '<polyline points="15 18 9 12 15 6"/>',
    forward: '<polyline points="9 18 15 12 9 6"/>',
    reload: '<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>',
    home: '<path d="M3 9.5 12 3l9 6.5V20a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 14 15 14 15 22"/>',
    zoomOut: '<circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><line x1="8" y1="11" x2="14" y2="11"/>',
    zoomIn: '<circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><line x1="8" y1="11" x2="14" y2="11"/><line x1="11" y1="8" x2="11" y2="14"/>',
    zoomReset: '<polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/>',
    copy: '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
    external: '<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/>',
    check: '<polyline points="20 6 9 17 4 12"/>',
    collapse: '<polyline points="13 17 18 12 13 7"/><polyline points="6 17 11 12 6 7"/>',
    expand: '<polyline points="11 17 6 12 11 7"/><polyline points="18 17 13 12 18 7"/>',
    close: '<line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>',
    ask: '<path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z"/>',
    chevronUp: '<polyline points="18 15 12 9 6 15"/>'
  };

  // Toolbar state survives in-page navigation within this window (session
  // storage is per-tab and per-origin). All access is guarded — some pages
  // run with storage disabled.
  var store = {
    get: function (k) { try { return sessionStorage.getItem(k); } catch (_) { return null; } },
    set: function (k, v) { try { sessionStorage.setItem(k, v); } catch (_) {} },
    del: function (k) { try { sessionStorage.removeItem(k); } catch (_) {} }
  };
  var POS_KEY = 'aiwb-toolbar-pos';
  var COL_KEY = 'aiwb-toolbar-collapsed';
  var ZOOM_KEY = 'aiwb-toolbar-zoom';

  function init() {
    if (document.getElementById('ai-workbench-toolbar-host')) return;
    if (!document.documentElement) return;

    var host = document.createElement('div');
    host.id = 'ai-workbench-toolbar-host';
    host.style.cssText =
      'all:initial;position:fixed;right:16px;bottom:16px;z-index:2147483647;';
    var root = host.attachShadow({ mode: 'open' });
    root.innerHTML =
      '<style>' +
      ':host{all:initial;}' +
      '.bar{display:flex;align-items:center;gap:1px;padding:5px 7px 5px 5px;background:rgba(17,24,39,0.85);-webkit-backdrop-filter:blur(14px);backdrop-filter:blur(14px);color:#e5e7eb;border:1px solid rgba(255,255,255,0.09);border-radius:12px;box-shadow:0 2px 6px rgba(0,0,0,0.25),0 12px 32px rgba(0,0,0,0.35);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;font-size:12px;user-select:none;-webkit-user-select:none;opacity:0.94;transition:opacity 0.2s,transform 0.15s,box-shadow 0.2s;}' +
      '.bar:hover{opacity:1;}' +
      '.bar.dragging{opacity:1;transform:scale(1.03);box-shadow:0 4px 10px rgba(0,0,0,0.3),0 18px 48px rgba(0,0,0,0.45);cursor:grabbing;}' +
      'button{appearance:none;-webkit-appearance:none;background:transparent;border:0;color:#d1d5db;width:30px;height:30px;border-radius:7px;cursor:pointer;display:inline-flex;align-items:center;justify-content:center;padding:0;transition:background 0.15s,color 0.15s,transform 0.1s;font-family:inherit;flex:none;}' +
      'button:hover:not(:disabled){background:rgba(255,255,255,0.12);color:#fff;}' +
      'button:active:not(:disabled){transform:scale(0.88);background:rgba(255,255,255,0.22);}' +
      'button:disabled{opacity:0.3;cursor:default;}' +
      'button:focus-visible{outline:2px solid #34d399;outline-offset:1px;}' +
      '.grip{cursor:grab;opacity:0.6;}' +
      '.grip:hover{opacity:1;}' +
      '.bar.dragging .grip{cursor:grabbing;}' +
      // All expanded controls live in one group so collapsing animates the
      // width smoothly. visibility is delayed until the collapse transition
      // finishes so the hidden buttons drop out of the tab order.
      '.exp{display:flex;align-items:center;gap:1px;max-width:720px;padding:3px 0;margin:0 3px;overflow:hidden;visibility:visible;transition:max-width 0.28s cubic-bezier(0.4,0,0.2,1),padding 0.28s cubic-bezier(0.4,0,0.2,1),margin 0.28s cubic-bezier(0.4,0,0.2,1),opacity 0.18s ease,visibility 0s;}' +
      '.bar.collapsed .exp{max-width:0;padding:0;margin:0;opacity:0;visibility:hidden;transition:max-width 0.28s cubic-bezier(0.4,0,0.2,1),padding 0.28s cubic-bezier(0.4,0,0.2,1),margin 0.28s cubic-bezier(0.4,0,0.2,1),opacity 0.18s ease,visibility 0s 0.25s;}' +
      '.sep{width:1px;height:16px;background:linear-gradient(rgba(255,255,255,0),rgba(255,255,255,0.25),rgba(255,255,255,0));margin:0 5px;flex:none;}' +
      '.badge{background:linear-gradient(135deg,#10b981,#0d9488);color:#fff;border-radius:6px;padding:2px 6px;font-size:10px;font-weight:700;letter-spacing:0.5px;margin:0 5px 0 3px;box-shadow:0 0 10px rgba(16,185,129,0.4);flex:none;}' +
      'button.zoom-display{width:auto;min-width:44px;color:#cbd5e1;font-size:11px;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:rgba(255,255,255,0.06);}' +
      'button.zoom-display:hover{background:rgba(255,255,255,0.14);color:#fff;}' +
      '.zoom-display.pulse{animation:aiwbPulse 0.35s ease;}' +
      '@keyframes aiwbPulse{50%{transform:scale(1.2);color:#34d399;}}' +
      '.url{max-width:150px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#94a3b8;padding:0 8px;height:30px;display:inline-flex;align-items:center;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:11px;cursor:pointer;border-radius:7px;transition:background 0.15s,color 0.15s;flex:none;}' +
      '.url:hover{color:#fff;background:rgba(255,255,255,0.1);}' +
      // Tip above the bar when Chromium shows chrome-error:// (VPN / Hosts / proxy).
      '.net-tip{display:none;max-width:min(420px,calc(100vw - 32px));margin:0 0 8px;padding:8px 10px;border-radius:10px;background:rgba(17,24,39,0.92);-webkit-backdrop-filter:blur(12px);backdrop-filter:blur(12px);border:1px solid rgba(255,255,255,0.1);color:#e2e8f0;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;font-size:11.5px;line-height:1.45;box-shadow:0 8px 24px rgba(0,0,0,0.35);}' +
      '.net-tip.is-on{display:block;}' +
      '.net-tip strong{color:#fbbf24;font-weight:600;}' +
      '.host-hidden .net-tip{display:none !important;}' +
      'button.restore{position:fixed;right:12px;bottom:12px;width:32px;height:32px;border-radius:50%;background:rgba(17,24,39,0.78);-webkit-backdrop-filter:blur(10px);backdrop-filter:blur(10px);border:1px solid rgba(255,255,255,0.08);color:#9ca3af;opacity:0.45;box-shadow:0 4px 16px rgba(0,0,0,0.3);transition:opacity 0.2s,background 0.2s,color 0.2s;}' +
      'button.restore:hover{opacity:1;color:#fff;background:rgba(17,24,39,0.95);}' +
      // Shown only while dragging: keeps pointer events flowing when the
      // cursor passes over cross-origin iframes (they would otherwise swallow
      // pointermove) and shows the grabbing cursor viewport-wide.
      '.drag-mask{position:fixed;inset:0;z-index:2147483647;display:none;cursor:grabbing;}' +
      '@media (prefers-reduced-motion:reduce){.bar,.exp,button,.zoom-display,button.restore{transition:none !important;animation:none !important;}}' +
      '</style>' +
      '<div class="net-tip" id="net-tip" role="status">' +
      '<strong>打不开？</strong>先确认已连公司 VPN，并检查本机 Hosts / 系统代理；也可点右侧「外开」用系统浏览器试同一地址。' +
      '</div>' +
      '<div class="bar" id="bar">' +
      '<button class="grip" id="grip" title="拖动工具栏（双击复位）" aria-label="拖动工具栏">' + svg(ICONS.grip) + '</button>' +
      '<span class="badge">AI</span>' +
      '<div class="exp">' +
      '<button id="back" title="后退" aria-label="后退">' + svg(ICONS.back) + '</button>' +
      '<button id="forward" title="前进" aria-label="前进">' + svg(ICONS.forward) + '</button>' +
      '<button id="reload" title="刷新" aria-label="刷新">' + svg(ICONS.reload) + '</button>' +
      '<button id="home" title="主页" aria-label="主页">' + svg(ICONS.home) + '</button>' +
      '<span class="sep"></span>' +
      '<button id="zoom-out" title="缩小" aria-label="缩小">' + svg(ICONS.zoomOut) + '</button>' +
      '<button class="zoom-display" id="zoom-display" title="缩放级别（点击重置）" aria-label="重置缩放">100%</button>' +
      '<button id="zoom-in" title="放大" aria-label="放大">' + svg(ICONS.zoomIn) + '</button>' +
      '<button id="zoom-reset" title="重置缩放" aria-label="重置缩放">' + svg(ICONS.zoomReset) + '</button>' +
      '<span class="sep"></span>' +
      '<span class="url" id="url" title="点击复制地址"></span>' +
      '<span class="sep"></span>' +
      '<button id="copy" title="复制地址" aria-label="复制地址">' + svg(ICONS.copy) + '</button>' +
      '<button id="external" title="在系统浏览器打开" aria-label="在系统浏览器打开">' + svg(ICONS.external) + '</button>' +
      '<span class="sep"></span>' +
      '<button id="ask" title="问天玑（送入选中文本，未选中则送页面标题+地址）" aria-label="问天玑">' + svg(ICONS.ask) + '</button>' +
      '</div>' +
      '<button id="collapse" title="收起工具栏" aria-label="收起工具栏">' + svg(ICONS.collapse) + '</button>' +
      '<button id="expand" title="展开工具栏" aria-label="展开工具栏" style="display:none;">' + svg(ICONS.expand) + '</button>' +
      '<button id="hide" title="隐藏工具栏" aria-label="隐藏工具栏">' + svg(ICONS.close) + '</button>' +
      '</div>' +
      '<button class="restore" id="restore" title="显示工具栏" aria-label="显示工具栏" style="display:none;">' + svg(ICONS.chevronUp) + '</button>' +
      '<div class="drag-mask" id="drag-mask"></div>';

    document.documentElement.appendChild(host);

    var bar = root.getElementById('bar');
    var back = root.getElementById('back');
    var forward = root.getElementById('forward');
    var reload = root.getElementById('reload');
    var home = root.getElementById('home');
    var zoomOut = root.getElementById('zoom-out');
    var zoomIn = root.getElementById('zoom-in');
    var zoomReset = root.getElementById('zoom-reset');
    var zoomDisplay = root.getElementById('zoom-display');
    var urlEl = root.getElementById('url');
    var copy = root.getElementById('copy');
    var external = root.getElementById('external');
    var ask = root.getElementById('ask');
    var collapse = root.getElementById('collapse');
    var expand = root.getElementById('expand');
    var hide = root.getElementById('hide');
    var restoreBtn = root.getElementById('restore');
    var dragMask = root.getElementById('drag-mask');
    var netTip = root.getElementById('net-tip');

    // ---- Zoom ----
    var zoomLevel = parseFloat(store.get(ZOOM_KEY));
    if (!isFinite(zoomLevel)) zoomLevel = 1.0;
    zoomLevel = Math.max(0.5, Math.min(3.0, zoomLevel));

    function applyZoom(pulse) {
      // WebView2 (Windows) and WKWebView (macOS) both support body.style.zoom.
      // This scales the page without reflowing layout — same as Ctrl+/- in
      // browsers. The toolbar host lives on documentElement, so it is not
      // affected by the body zoom.
      try {
        document.body.style.zoom = zoomLevel;
      } catch (e) {
        // Last-resort fallback: transform scale (will reflow and miss scrollbars)
        document.documentElement.style.transform = 'scale(' + zoomLevel + ')';
        document.documentElement.style.transformOrigin = '0 0';
      }
      zoomDisplay.textContent = Math.round(zoomLevel * 100) + '%';
      store.set(ZOOM_KEY, String(zoomLevel));
      if (pulse) {
        zoomDisplay.classList.remove('pulse');
        void zoomDisplay.offsetWidth; // restart the CSS animation
        zoomDisplay.classList.add('pulse');
      }
    }
    function setZoom(level) {
      zoomLevel = Math.max(0.5, Math.min(3.0, Math.round(level * 10) / 10));
      applyZoom(true);
    }
    zoomIn.addEventListener('click', function () { setZoom(zoomLevel + 0.1); });
    zoomOut.addEventListener('click', function () { setZoom(zoomLevel - 0.1); });
    zoomReset.addEventListener('click', function () { setZoom(1.0); });
    zoomDisplay.addEventListener('click', function () { setZoom(1.0); });

    // Chromium error interstitial (ERR_CONNECTION_CLOSED etc.) uses chrome-error://
    // and often shows chromewebdata in the address strip.
    function isChromeErrorPage() {
      try {
        var href = String(location.href || '');
        return location.protocol === 'chrome-error:'
          || href.indexOf('chrome-error://') === 0
          || href.indexOf('chromewebdata') !== -1;
      } catch (_) {
        return false;
      }
    }

    // ---- URL state ----
    function updateState() {
      // 展示精简地址（去掉协议和默认端口，只留域名+路径），完整地址见 title / 复制按钮
      var short = location.host + (location.pathname === '/' ? '' : location.pathname);
      urlEl.textContent = short; // CSS ellipsis handles truncation
      urlEl.title = location.href;
      back.disabled = window.history.length <= 1;
      if (netTip) netTip.classList.toggle('is-on', isChromeErrorPage());
    }
    window.addEventListener('popstate', updateState);
    window.addEventListener('hashchange', updateState);
    window.addEventListener('pageshow', updateState); // bfcache restores

    // ---- Navigation ----
    back.addEventListener('click', function () {
      if (window.history.length > 1) window.history.back();
    });
    forward.addEventListener('click', function () {
      window.history.forward();
    });
    reload.addEventListener('click', function () {
      location.reload();
    });
    home.addEventListener('click', function () {
      // Go to site root. location.origin works for http(s) and is always
      // same-origin.
      location.href = location.origin + '/';
    });

    // ---- Copy URL ----
    function fallbackCopy() {
      var ta = document.createElement('textarea');
      ta.value = location.href;
      ta.style.cssText = 'position:fixed;top:-9999px;opacity:0;';
      document.body.appendChild(ta);
      ta.select();
      try { document.execCommand('copy'); } catch (_) {}
      document.body.removeChild(ta);
    }
    function flashBtn(btn, restoreIcon) {
      btn.innerHTML = svg(ICONS.check);
      setTimeout(function () { btn.innerHTML = svg(restoreIcon); }, 900);
    }
    function copyURL() {
      var done = function () { flashBtn(copy, ICONS.copy); };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(location.href).then(done, function () {
          fallbackCopy();
          done();
        });
      } else {
        fallbackCopy();
        done();
      }
    }
    urlEl.addEventListener('click', copyURL);
    copy.addEventListener('click', copyURL);

    // ---- External browser ----
    // The toolbar runs inside an external-URL webview, where Tauri does not
    // inject the `__TAURI_INTERNALS__` IPC bridge — so we can't `invoke`
    // the shell plugin directly. Instead we trigger a navigation to a
    // magic placeholder hostname (`aiwb-shell.open`) carrying the target
    // URL as a query param. The `on_navigation` handler on the Rust side
    // intercepts this host, calls `shell.open(target)`, and returns false
    // so the webview never actually fetches the placeholder. Fallback to
    // copying the URL if the trigger somehow fails.
    external.addEventListener('click', function () {
      // Magic host must stay in sync with SHELL_OPEN_TRIGGER_HOST in Rust.
      var a = document.createElement('a');
      a.href = 'https://aiwb-shell.open/?url=' + encodeURIComponent(location.href);
      a.style.cssText = 'display:none;position:fixed;top:-9999px;left:-9999px;';
      document.body.appendChild(a);
      a.click();
      setTimeout(function () {
        if (a.parentNode) a.parentNode.removeChild(a);
      }, 200);
      // Also copy URL as backup in case the webview doesn't fire on_navigation
      copyURL();
      flashBtn(external, ICONS.external);
    });

    // ---- Ask ai-hub --------------------------------------------------
    // Sends the current selection (or the page title + URL when nothing is
    // selected) to the ai-hub window through the `aiwb-aihub.open`
    // magic host — same no-IPC navigation trick as the external button.
    // 3500 chars keeps the encoded URL well inside navigation limits.
    ask.addEventListener('click', function () {
      var text = '';
      try { text = (window.getSelection() || '').toString(); } catch (_) {}
      text = text.trim();
      if (!text) text = document.title + '\n' + location.href;
      if (text.length > 3500) text = text.slice(0, 3500);
      var a = document.createElement('a');
      a.href = 'https://aiwb-aihub.open/?text=' + encodeURIComponent(text);
      a.style.cssText = 'display:none;position:fixed;top:-9999px;left:-9999px;';
      document.body.appendChild(a);
      a.click();
      setTimeout(function () {
        if (a.parentNode) a.parentNode.removeChild(a);
      }, 200);
      flashBtn(ask, ICONS.ask);
    });

    // ---- Collapse / expand ----
    function setCollapsed(v) {
      bar.classList.toggle('collapsed', v);
      collapse.style.display = v ? 'none' : '';
      expand.style.display = v ? '' : 'none';
      store.set(COL_KEY, v ? '1' : '0');
      if (!v && dragged) expandKeepInView();
    }
    // When dragged (left/top-anchored) close to the right edge, expanding
    // grew the bar off-screen: dragging clamps with the *collapsed* width,
    // so the bar can sit flush against the border, and expanding pivots on
    // the immobile left edge. Shift the host left so the full expanded bar
    // fits — estimated up-front so the animation grows leftward, then
    // re-clamped with the real width once the width transition settles.
    function expandKeepInView() {
      var exp = bar.querySelector('.exp');
      if (!exp) return;
      var target = bar.offsetWidth + exp.scrollWidth + 6 - exp.offsetWidth;
      var overflow = host.getBoundingClientRect().left + target - window.innerWidth;
      if (overflow > 0) {
        host.style.left = Math.max(EDGE_GAP, host.getBoundingClientRect().left - overflow) + 'px';
      }
      setTimeout(clampIntoView, 300);
    }
    // Report the toggle to the shell via a magic-host navigation so the
    // choice is remembered globally (across windows, sites and app
    // restarts). Same no-IPC trick as the external-browser button.
    function reportCollapsed(v) {
      var a = document.createElement('a');
      a.href = 'https://aiwb-toolbar.open/?collapsed=' + (v ? '1' : '0');
      a.style.cssText = 'display:none;position:fixed;top:-9999px;left:-9999px;';
      (document.body || document.documentElement).appendChild(a);
      a.click();
      setTimeout(function () { if (a.parentNode) a.parentNode.removeChild(a); }, 200);
    }
    collapse.addEventListener('click', function () { setCollapsed(true); reportCollapsed(true); });
    expand.addEventListener('click', function () { setCollapsed(false); reportCollapsed(false); });

    // ---- Hide / restore ----
    // Hiding only hides the bar; a small restore dot appears in the corner
    // so the toolbar is never lost forever (previously it only came back
    // after a page navigation).
    hide.addEventListener('click', function () {
      bar.style.display = 'none';
      host.classList.add('host-hidden');
      restoreBtn.style.display = 'inline-flex';
    });
    restoreBtn.addEventListener('click', function () {
      bar.style.display = '';
      host.classList.remove('host-hidden');
      restoreBtn.style.display = 'none';
      if (netTip) netTip.classList.toggle('is-on', isChromeErrorPage());
    });

    // ---- Positioning: drag, re-dock, clamp, persist ----
    var dragging = false;
    var dragged = false; // true once the toolbar was dragged or restored
    var startX = 0, startY = 0, startLeft = 0, startTop = 0, lastDown = 0;
    // Minimum gap between the bar and the viewport edges — never fully
    // flush, collapsed or expanded.
    var EDGE_GAP = 8;

    function clampIntoView() {
      var rect = host.getBoundingClientRect();
      var newLeft = Math.min(rect.left, window.innerWidth - EDGE_GAP - host.offsetWidth);
      var newTop = Math.min(rect.top, window.innerHeight - EDGE_GAP - host.offsetHeight);
      if (newLeft < EDGE_GAP) newLeft = EDGE_GAP;
      if (newTop < EDGE_GAP) newTop = EDGE_GAP;
      host.style.left = newLeft + 'px';
      host.style.top = newTop + 'px';
    }
    function anchorToCorner() {
      // Back to the default CSS anchoring (right/bottom 16px).
      dragged = false;
      host.style.left = '';
      host.style.top = '';
      host.style.right = '16px';
      host.style.bottom = '16px';
      store.del(POS_KEY);
    }

    function onMove(e) {
      if (!dragging) return;
      var newLeft = Math.max(EDGE_GAP, Math.min(window.innerWidth - EDGE_GAP - host.offsetWidth, startLeft + (e.clientX - startX)));
      var newTop = Math.max(EDGE_GAP, Math.min(window.innerHeight - EDGE_GAP - host.offsetHeight, startTop + (e.clientY - startY)));
      host.style.left = newLeft + 'px';
      host.style.top = newTop + 'px';
    }
    function onUp() {
      if (!dragging) return;
      dragging = false;
      bar.classList.remove('dragging');
      dragMask.style.display = 'none';
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', onUp);
      document.removeEventListener('pointercancel', onUp);
      store.set(POS_KEY, JSON.stringify({ left: host.style.left, top: host.style.top }));
    }

    // The whole bar is a drag surface, except for interactive controls.
    // Pointer events (not mouse events) so touch/pen dragging works too.
    // A quick second press on the bar background (or grip) re-docks the
    // toolbar to the bottom-right corner — implemented manually because
    // preventDefault on pointerdown can suppress native dblclick.
    bar.addEventListener('pointerdown', function (e) {
      if (e.button !== 0) return;
      if (e.target.closest('button:not(.grip), .url')) return;
      var now = Date.now();
      if (now - lastDown < 300) {
        lastDown = 0;
        anchorToCorner();
        return;
      }
      lastDown = now;
      e.preventDefault();
      dragging = true;
      dragged = true;
      startX = e.clientX;
      startY = e.clientY;
      var rect = host.getBoundingClientRect();
      // Switch from right/bottom anchoring to left/top so the host follows
      // the cursor freely.
      host.style.right = 'auto';
      host.style.bottom = 'auto';
      host.style.left = rect.left + 'px';
      host.style.top = rect.top + 'px';
      startLeft = rect.left;
      startTop = rect.top;
      bar.classList.add('dragging');
      dragMask.style.display = 'block';
      document.addEventListener('pointermove', onMove);
      document.addEventListener('pointerup', onUp);
      document.addEventListener('pointercancel', onUp);
    });

    // Re-clamp position when the window resizes — but ONLY in dragged mode.
    //  - Default (never dragged): the host keeps its right/bottom:16px CSS
    //    anchoring, so the browser keeps it glued to the bottom-right corner
    //    through maximize/restore with zero JS involvement. Repositioning
    //    from JS here would freeze it at old coordinates (it "drifts" toward
    //    mid-screen when the window grows), and pinning left/top without
    //    clearing right/bottom over-constrains the box and collapsed it to
    //    0x0 (the earlier "toolbar disappears" bug).
    //  - Dragged: the host is left/top-anchored (right/bottom = auto), so we
    //    just re-clamp those coordinates into the viewport.
    window.addEventListener('resize', function () {
      if (dragged) clampIntoView();
    });

    // ---- Restore persisted state (position / collapsed / zoom) ----
    var saved = null;
    try { saved = JSON.parse(store.get(POS_KEY) || 'null'); } catch (_) {}
    if (saved && isFinite(parseFloat(saved.left)) && isFinite(parseFloat(saved.top))) {
      host.style.right = 'auto';
      host.style.bottom = 'auto';
      host.style.left = String(saved.left);
      host.style.top = String(saved.top);
      dragged = true;
      clampIntoView();
    }
    // Collapsed state: this window's own sessionStorage wins (keeps the
    // toolbar stable across in-page navigation); with no local record yet,
    // fall back to the globally remembered choice injected by Rust.
    var savedCol = store.get(COL_KEY);
    if (savedCol === '1') setCollapsed(true);
    else if (savedCol === null && window.__aiwbToolbarDefaultCollapsed) setCollapsed(true);
    if (zoomLevel !== 1) applyZoom(false);
    updateState();
  }

  // Userscript runner — injected pages call this from their own IIFE so they
  // execute after the toolbar sets up, with a guarded try/catch so one bad
  // script can't break the toolbar or other scripts.
  //
  // `patterns` is re-checked against the current URL because this init script runs on
  // every navigation, not just the first. The glob rules mirror the app's own matcher
  // (src/lib/webTools.ts): `*` any run, `?` one char, everything else literal.
  window.__aiwb_url_matches = function (patterns) {
    if (!patterns || !patterns.length) return true;
    var url = String(location.href);
    for (var i = 0; i < patterns.length; i++) {
      var p = String(patterns[i] || '').trim();
      if (p === '<all_urls>') return true;
      if (!p) continue;
      var re = '';
      for (var j = 0; j < p.length; j++) {
        var ch = p.charAt(j);
        if (ch === '*') re += '.*';
        else if (ch === '?') re += '.';
        else if ('+.^${}()|[]\\'.indexOf(ch) >= 0) re += '\\' + ch;
        else re += ch;
      }
      try {
        if (new RegExp('^' + re + '$').test(url)) return true;
      } catch (e) {
        /* a broken pattern simply never matches */
      }
    }
    return false;
  };

  window.__aiwb_run_userscript = function (fn, patterns) {
    if (!window.__aiwb_url_matches(patterns)) return;
    try {
      if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', fn);
      } else {
        fn();
      }
    } catch (e) {
      console.warn('[AI Workbench] userscript error:', e);
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
"#;

/// Open a browser window with a floating nav toolbar (back / forward / reload /
/// copy URL / zoom / drag). Mirrors the JS-side `createBrowserWindow` flow but
/// uses `WebviewWindowBuilder::initialization_script` so the toolbar survives
/// in-page navigation (a plain `eval` would be dropped on the next page load).
///
/// Marked `async` because Tauri docs warn that `WebviewWindowBuilder::build`
/// deadlocks in synchronous Tauri commands on Windows — async commands run on
/// a separate task and avoid the deadlock.
#[tauri::command]
pub async fn open_browser_window_with_toolbar(
    app: tauri::AppHandle,
    url: String,
    width: Option<f64>,
    height: Option<f64>,
    label: Option<String>,
    userscripts: Option<Vec<UserscriptInit>>,
    // Frontend-mapped: "light" / "dark" / None|"system" = follow OS.
    // Do not inherit main.theme() — main is undecorated and reports OS light,
    // ignoring glass/ice/silver.
    theme: Option<String>,
) -> Result<String, String> {
    if !is_safe_url(&url) {
        return Err("仅允许 http/https 地址".to_string());
    }
    let parsed: Url = url.parse().map_err(|e| format!("无效 URL: {}", e))?;
    let label = label.unwrap_or_else(|| "browser".to_string());

    // Reuse an existing window with the same label — matches the JS-side
    // preexisting-window recovery path. We don't navigate here because the
    // caller (browser.ts) already handles focus-only when the key is in the
    // session map; this path only runs on a fresh create.
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(label);
    }

    let w = width.unwrap_or(900.0);
    let h = height.unwrap_or(700.0);
    let title = parsed
        .host_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Browser".to_string());

    // The on_navigation handler intercepts clicks on the toolbar's
    // "external browser" button. The toolbar sets an <a> href to
    // `https://aiwb-shell.open/?url=<encoded>` and clicks it; we catch the
    // navigation here, hand the encoded target URL to `shell.open`, and
    // return false to stop the webview from actually loading the placeholder
    // host. This avoids the need for an IPC bridge (which Tauri does not
    // inject into external-URL webviews).
    let app_handle_for_nav = app.clone();

    // Native title-bar theme comes from the app setting (mapped on the JS
    // side). Falling back to main.theme() would stick to the OS light bar
    // while the workbench itself uses glass/ice/silver.
    let window_theme: Option<Theme> = match theme.as_deref() {
        Some("light") => Some(Theme::Light),
        Some("dark") => Some(Theme::Dark),
        Some("system") | None => None,
        _ => Some(Theme::Dark),
    };

    // Build combined initialization script: remembered collapsed state +
    // toolbar + matching userscripts. The state goes first so the toolbar
    // IIFE reads it during init.
    let collapsed_default = toolbar_collapsed_state(&app).load(Ordering::Relaxed);
    let mut init_script = format!("window.__aiwbToolbarDefaultCollapsed = {};\n", collapsed_default);
    init_script.push_str(BROWSER_TOOLBAR_INIT_JS);
    if let Some(scripts) = userscripts {
        for script in scripts {
            if script.code.trim().is_empty() {
                continue;
            }
            let patterns_json = serde_json::to_string(&script.patterns).unwrap_or_else(|_| "[]".into());
            init_script.push_str("\n(function(){\n");
            init_script.push_str("// Userscript: ");
            // The name comes from the script itself (`@name` on import) and this is a
            // line comment inside generated code — a newline in it would escape.
            init_script.push_str(&script.name.replace(['\r', '\n'], " "));
            init_script.push('\n');
            init_script.push_str("__aiwb_run_userscript(() => {\n");
            init_script.push_str(&script.code);
            init_script.push_str("\n}, ");
            init_script.push_str(&patterns_json);
            init_script.push_str(");\n})();\n");
        }
    }

    let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title(title)
        .inner_size(w, h)
        .min_inner_size(640.0, 480.0)
        .resizable(true)
        .decorations(true)
        .focused(true)
        .theme(window_theme)
        // On Windows, Tauri's default drag-drop handler disables HTML5 drag
        // and drop APIs in the WebView2. These popup windows are plain web
        // tools; page-internal DnD (drag-sort boards, whiteboards, upload
        // dropzones) matters more than dragging OS files onto the page, so
        // disable the handler to restore normal DnD.
        .disable_drag_drop_handler()
        .initialization_script(&init_script)
        .on_navigation(move |url: &Url| {
            if url.host_str() == Some(SHELL_OPEN_TRIGGER_HOST) {
                for (k, v) in url.query_pairs() {
                    if k == "url" {
                        let target = v.to_string();
                        // 官方建议改用 tauri-plugin-opener；不新增插件依赖，先用旧接口。
                        #[allow(deprecated)]
                        let _ = app_handle_for_nav.shell().open(target, None);
                    }
                }
                return false;
            }
            if url.host_str() == Some(AI_HUB_TRIGGER_HOST) {
                for (k, v) in url.query_pairs() {
                    if k == "text" {
                        let text = v.to_string();
                        if !text.trim().is_empty() {
                            let _ = crate::tray::open_ai_hub_with_text(
                                app_handle_for_nav.clone(),
                                text,
                                None,
                            );
                        }
                    }
                }
                return false;
            }
            if url.host_str() == Some(TOOLBAR_STATE_TRIGGER_HOST) {
                let mut collapsed: Option<bool> = None;
                for (k, v) in url.query_pairs() {
                    if k == "collapsed" {
                        collapsed = Some(v == "1");
                    }
                }
                if let Some(c) = collapsed {
                    toolbar_collapsed_state(&app_handle_for_nav).store(c, Ordering::Relaxed);
                    persist_toolbar_collapsed(&app_handle_for_nav, c);
                }
                return false;
            }
            true
        })
        .build()
        .map_err(|e| format!("创建浏览器窗口失败: {}", e))?;

    let _ = win.set_focus();
    Ok(label)
}

/// Largest script body the importer will hold in memory.
const MAX_FETCH_BYTES: u64 = 2 * 1024 * 1024;

/// Fetch a userscript source for the "import from URL" flow.
///
/// It has to happen here rather than with the webview's own `fetch()`: script hosts
/// (GreasyFork above all) do not send `Access-Control-Allow-Origin`, so the browser-side
/// request is blocked by CORS and the feature could never work for its main use case.
#[tauri::command]
pub async fn fetch_userscript_source(url: String) -> Result<String, String> {
    if !is_safe_url(&url) {
        return Err("仅支持 http/https 地址".to_string());
    }
    let client = reqwest::Client::builder()
        // Some hosts reject reqwest's default agent outright.
        .user_agent("Mozilla/5.0 (AI-Workbench) userscript-import")
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("无法发起请求: {}", e))?;
    let resp = client
        .get(url.as_str())
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("服务端返回 {}", status.as_str()));
    }
    if resp.content_length().unwrap_or(0) > MAX_FETCH_BYTES {
        return Err("文件超过 2MB，不像脚本文件".to_string());
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;
    if text.len() as u64 > MAX_FETCH_BYTES {
        return Err("文件超过 2MB，不像脚本文件".to_string());
    }
    if text.trim().is_empty() {
        return Err("返回内容为空".to_string());
    }
    Ok(text)
}
