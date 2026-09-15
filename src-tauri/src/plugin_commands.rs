use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
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

fn is_safe_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
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
/// page load via `WebviewWindowBuilder::initialization_script`, so it survives
/// in-page navigation. Uses Shadow DOM to avoid colliding with the host page's
/// styles. Toolbar features:
///   - Drag handle (move toolbar anywhere in the viewport)
///   - Back / Forward / Reload / Home
///   - Zoom out / display / zoom in / reset (WebView2 supports body.style.zoom)
///   - URL display (click to copy)
///   - Copy URL / Open in external browser / Collapse / Hide
///
/// The toolbar calls the webview's own `history.back()/forward()` and
/// `location.reload()` — no IPC round-trip needed for nav actions. Opening
/// the external browser tries `window.open` first; if the webview blocks it,
/// we fall back to copying the URL so the user can paste it into their
/// system browser.
const BROWSER_TOOLBAR_INIT_JS: &str = r#"(function () {
  if (window.__aiWorkbenchToolbarInjected) return;
  Object.defineProperty(window, '__aiWorkbenchToolbarInjected', {
    value: true,
    writable: false,
    configurable: false,
  });

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
      '.bar{display:flex;gap:2px;align-items:center;padding:4px 6px;background:#1f2937;color:#fff;border-radius:10px;box-shadow:0 8px 24px rgba(0,0,0,0.4);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;font-size:12px;user-select:none;}' +
      '.bar.collapsed .expanded-only{display:none !important;}' +
      'button{appearance:none;background:transparent;border:0;color:#fff;width:28px;height:28px;border-radius:5px;cursor:pointer;display:inline-flex;align-items:center;justify-content:center;padding:0;font-size:14px;line-height:1;transition:background 0.15s;font-family:inherit;}' +
      'button:hover:not(:disabled){background:rgba(255,255,255,0.18);}' +
      'button:active:not(:disabled){background:rgba(255,255,255,0.3);}' +
      'button:disabled{opacity:0.35;cursor:default;}' +
      '.grip{cursor:grab;opacity:0.55;font-size:14px;letter-spacing:-3px;}' +
      '.grip:hover{opacity:0.9;background:rgba(255,255,255,0.12);}' +
      '.grip:active{cursor:grabbing;}' +
      '.sep{width:1px;height:18px;background:rgba(255,255,255,0.2);margin:0 3px;}' +
      '.url{max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#9ca3af;padding:0 6px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px;cursor:text;border-radius:4px;}' +
      '.url:hover{color:#fff;background:rgba(255,255,255,0.08);}' +
      '.badge{background:#10b981;color:#fff;border-radius:3px;padding:1px 5px;font-size:10px;margin:0 4px 0 2px;font-weight:600;letter-spacing:0.5px;}' +
      '.zoom-display{min-width:34px;text-align:center;color:#d1d5db;font-size:11px;padding:0 2px;font-family:ui-monospace,monospace;}' +
      '</style>' +
      '<div class="bar" id="bar">' +
      '<button class="grip" id="grip" title="拖动工具栏">::</button>' +
      '<span class="badge">AI</span>' +
      '<button class="expanded-only" id="back" title="后退">←</button>' +
      '<button class="expanded-only" id="forward" title="前进">→</button>' +
      '<button class="expanded-only" id="reload" title="刷新">⟳</button>' +
      '<button class="expanded-only" id="home" title="主页">⌂</button>' +
      '<span class="sep expanded-only"></span>' +
      '<button class="expanded-only" id="zoom-out" title="缩小">−</button>' +
      '<span class="zoom-display expanded-only" id="zoom-display">100%</span>' +
      '<button class="expanded-only" id="zoom-in" title="放大">+</button>' +
      '<button class="expanded-only" id="zoom-reset" title="重置缩放" style="font-size:12px;">⊙</button>' +
      '<span class="sep expanded-only"></span>' +
      '<span class="url expanded-only" id="url" title=""></span>' +
      '<span class="sep expanded-only"></span>' +
      '<button class="expanded-only" id="copy" title="复制地址">⧉</button>' +
      '<button class="expanded-only" id="external" title="在系统浏览器打开">↗</button>' +
      '<button class="expanded-only" id="collapse" title="折叠" style="font-size:10px;">▁</button>' +
      '<button id="expand" title="展开" style="display:none;font-size:10px;">▮</button>' +
      '<button id="hide" title="隐藏工具栏">×</button>' +
      '</div>';

    document.documentElement.appendChild(host);

    var bar = root.getElementById('bar');
    var grip = root.getElementById('grip');
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
    var collapse = root.getElementById('collapse');
    var expand = root.getElementById('expand');
    var hide = root.getElementById('hide');

    var zoomLevel = 1.0;
    function applyZoom() {
      // WebView2 (Windows) and WKWebView (macOS) both support body.style.zoom.
      // This scales the page without reflowing layout — same as Ctrl+/- in browsers.
      try {
        document.body.style.zoom = zoomLevel;
      } catch (e) {
        // Last-resort fallback: transform scale (will reflow and miss scrollbars)
        document.documentElement.style.transform = 'scale(' + zoomLevel + ')';
        document.documentElement.style.transformOrigin = '0 0';
      }
      zoomDisplay.textContent = Math.round(zoomLevel * 100) + '%';
    }

    function updateState() {
      urlEl.textContent = location.href.length > 40 ? location.href.slice(0, 40) + '…' : location.href;
      urlEl.title = location.href;
      back.disabled = window.history.length <= 1;
    }

    // ---- Navigation actions ----
    back.addEventListener('click', function (e) {
      e.preventDefault();
      if (window.history.length > 1) window.history.back();
    });
    forward.addEventListener('click', function (e) {
      e.preventDefault();
      window.history.forward();
    });
    reload.addEventListener('click', function (e) {
      e.preventDefault();
      location.reload();
    });
    home.addEventListener('click', function (e) {
      e.preventDefault();
      // Go to site root. location.origin works for http(s) and is always same-origin.
      location.href = location.origin + '/';
    });

    // ---- Zoom ----
    zoomIn.addEventListener('click', function (e) {
      e.preventDefault();
      zoomLevel = Math.min(Math.round((zoomLevel + 0.1) * 10) / 10, 3.0);
      applyZoom();
    });
    zoomOut.addEventListener('click', function (e) {
      e.preventDefault();
      zoomLevel = Math.max(Math.round((zoomLevel - 0.1) * 10) / 10, 0.5);
      applyZoom();
    });
    zoomReset.addEventListener('click', function (e) {
      e.preventDefault();
      zoomLevel = 1.0;
      applyZoom();
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
    function copyURL() {
      var done = function () {
        copy.textContent = '✓';
        setTimeout(function () { copy.textContent = '⧉'; }, 1000);
      };
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
    urlEl.addEventListener('click', function (e) {
      e.preventDefault();
      copyURL();
    });
    copy.addEventListener('click', function (e) {
      e.preventDefault();
      copyURL();
    });

    // ---- External browser ----
    // The toolbar runs inside an external-URL webview, where Tauri does not
    // inject the `__TAURI_INTERNALS__` IPC bridge — so we can't `invoke`
    // the shell plugin directly. Instead we trigger a navigation to a
    // magic placeholder hostname (`aiwb-shell.open`) carrying the target
    // URL as a query param. The `on_navigation` handler on the Rust side
    // intercepts this host, calls `shell.open(target)`, and returns false
    // so the webview never actually fetches the placeholder. Fallback to
    // copying the URL if the trigger somehow fails.
    function flashOK() {
      external.textContent = '✓';
      setTimeout(function () { external.textContent = '↗'; }, 1000);
    }
    external.addEventListener('click', function (e) {
      e.preventDefault();
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
      flashOK();
    });

    // ---- Collapse / expand ----
    collapse.addEventListener('click', function (e) {
      e.preventDefault();
      bar.classList.add('collapsed');
      collapse.style.display = 'none';
      expand.style.display = '';
    });
    expand.addEventListener('click', function (e) {
      e.preventDefault();
      bar.classList.remove('collapsed');
      collapse.style.display = '';
      expand.style.display = 'none';
    });
    hide.addEventListener('click', function (e) {
      e.preventDefault();
      host.style.display = 'none';
    });

    // ---- Dragging ----
    // The grip is the only drag handle. On mousedown we switch from
    // right/bottom anchoring to left/top so the host follows the cursor
    // freely. Position is clamped to the viewport so the toolbar can't be
    // dragged off-screen.
    var dragging = false;
    var startX = 0, startY = 0, startLeft = 0, startTop = 0;
    function onMove(e) {
      if (!dragging) return;
      var newLeft = startLeft + (e.clientX - startX);
      var newTop = startTop + (e.clientY - startY);
      newLeft = Math.max(0, Math.min(window.innerWidth - host.offsetWidth, newLeft));
      newTop = Math.max(0, Math.min(window.innerHeight - host.offsetHeight, newTop));
      host.style.left = newLeft + 'px';
      host.style.top = newTop + 'px';
    }
    function onUp() {
      dragging = false;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
    }
    grip.addEventListener('mousedown', function (e) {
      // Ignore right/middle clicks; only left button drags
      if (e.button !== 0) return;
      e.preventDefault();
      dragging = true;
      startX = e.clientX;
      startY = e.clientY;
      var rect = host.getBoundingClientRect();
      host.style.right = 'auto';
      host.style.bottom = 'auto';
      host.style.left = rect.left + 'px';
      host.style.top = rect.top + 'px';
      startLeft = rect.left;
      startTop = rect.top;
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
    });

    // Re-clamp position if the window resizes under the toolbar
    window.addEventListener('resize', function () {
      var rect = host.getBoundingClientRect();
      var newLeft = Math.min(rect.left, window.innerWidth - host.offsetWidth);
      var newTop = Math.min(rect.top, window.innerHeight - host.offsetHeight);
      if (newLeft < 0) newLeft = 0;
      if (newTop < 0) newTop = 0;
      host.style.left = newLeft + 'px';
      host.style.top = newTop + 'px';
    });

    window.addEventListener('popstate', updateState);
    window.addEventListener('hashchange', updateState);
    updateState();
  }

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
    let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title(title)
        .inner_size(w, h)
        .min_inner_size(640.0, 480.0)
        .resizable(true)
        .decorations(true)
        .focused(true)
        .initialization_script(BROWSER_TOOLBAR_INIT_JS)
        .on_navigation(move |url: &Url| {
            if url.host_str() == Some(SHELL_OPEN_TRIGGER_HOST) {
                for (k, v) in url.query_pairs() {
                    if k == "url" {
                        let target = v.to_string();
                        let _ = app_handle_for_nav.shell().open(target, None);
                    }
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
