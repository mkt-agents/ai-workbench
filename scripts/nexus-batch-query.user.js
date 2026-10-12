/**
 * Nexus 2 批量查询服务（AI Workbench 网页工具 · 用户脚本）
 *
 * 用法：网页工具页 → 脚本 → 新建脚本
 *   名称：Nexus 批量查询服务
 *   匹配规则：填你的 Nexus 地址，如  http://10.0.0.1/nexus/*  （也可 all_urls，脚本会自检不在 Nexus 页面就不激活）
 *   代码：粘贴本文件全文
 *
 * 功能：在 Nexus 2 页面右上角注入悬浮面板，粘贴多行构件名批量查询最新 RELEASE 版本。
 *   - 每行一个关键词（如 demo.shop.parent.pom），或 group:artifact 精确查（如 com.example.shop:demo.shop.account.sdk）
 *   - 走 Nexus 自带 REST：/service/local/lucene/search，登录态自动带上（同源 fetch）
 *   - 结果按输入顺序呈现，查询中/失败/无结果三态分明，失败行可单行重试；未查询时列表区显示引导空态
 *   - 最新版本点击复制 g:a:v；发布时间优先 lucene，缺则补查仓库 content 目录的 lastModified（与详情页 Uploaded Date 同源）；升级版本点击复制 artifactId+新版本
 *   - 一键复制全部（TSV）/ 一键复制 artifactId+升级版本（TSV）
 *   - 并发 4，Ctrl+Enter 直接查询；输入内容与面板位置自动记忆（localStorage）
 *   - 面板可拖动 / 右下角改尺寸 / 最小化；宽高与位置记忆；窄屏自动收紧
 *   - 深色磨砂：版本胶囊、仓库+命中合并、日期精简；隔离宿主页 text-decoration 干扰
 */
(function () {
  if (window.__aiwbNexusBatch) return;
  window.__aiwbNexusBatch = true;

  // 不在 Nexus 页面（含反向代理子路径）就不激活
  if (!/\/nexus(\/|$)/.test(location.pathname) && !document.querySelector('img[src*="nexus"], a[href*="/nexus/"]')) {
    return;
  }

  var LS_KEY = 'aiwbNexusBatchState';

  function loadState() {
    try { return JSON.parse(localStorage.getItem(LS_KEY)) || {}; } catch (e) { return {}; }
  }
  function saveState(patch) {
    try { localStorage.setItem(LS_KEY, JSON.stringify(Object.assign(loadState(), patch))); } catch (e) { /* 隐私模式等 */ }
  }

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function nexusBase() {
    return /^\/nexus(\/|$)/.test(location.pathname) ? location.origin + '/nexus' : location.origin;
  }

  function cmpVersion(a, b) {
    var ta = String(a || '').split(/[.\-_]/), tb = String(b || '').split(/[.\-_]/);
    for (var i = 0; i < Math.max(ta.length, tb.length); i++) {
      var x = ta[i], y = tb[i];
      var nx = /^\d+$/.test(x || ''), ny = /^\d+$/.test(y || '');
      if (nx && ny) { var d = Number(x) - Number(y); if (d) return d; }
      else if (nx !== ny) return nx ? 1 : -1;              // 数字段 > 缺段
      else if ((x || '') !== (y || '')) {                   // 字段名：RELEASE 优先于 SNAPSHOT
        if (/release/i.test(x) && /snapshot/i.test(y)) return 1;
        if (/release/i.test(y) && /snapshot/i.test(x)) return -1;
        return (x || '') < (y || '') ? -1 : 1;
      }
    }
    return 0;
  }

  // 预设升级版本：末段 +1，满 999 归零并向前一段进位（2.4.999-RELEASE → 2.5.0-RELEASE，2.999.999 → 3.0.0）。
  // 最左段不再进位、直接增长（999.999 → 1000.0），避免出现 "1.0.0.0" 这种凭空多一段的版本。
  var SEG_MAX = 999;
  function bumpVersion(v) {
    var m = String(v || '').match(/^(\d+(?:\.\d+)*)(.*)$/);
    if (!m) return v;
    var nums = m[1].split('.');
    nums[nums.length - 1] = String(Number(nums[nums.length - 1]) + 1);
    for (var i = nums.length - 1; i > 0; i--) {
      if (Number(nums[i]) <= SEG_MAX) break;
      nums[i] = String(Number(nums[i]) - (SEG_MAX + 1));
      nums[i - 1] = String(Number(nums[i - 1]) + 1);
    }
    return nums.join('.') + (m[2] || '');
  }

  // 时间：lucene/collapseresults 常不带 timestamp；详情页的 Uploaded Date 来自仓库 content 的 lastModified。
  function parseTime(t) {
    if (t == null || t === '') return 0;
    var n = Number(t);
    if (!isNaN(n) && n > 0) {
      // 秒级时间戳补成毫秒
      if (n < 1e12) n *= 1000;
      return n;
    }
    var ms = Date.parse(String(t));
    return isNaN(ms) ? 0 : ms;
  }
  function tsOf(o) {
    if (!o) return 0;
    return parseTime(
      o.lastModified != null ? o.lastModified
        : (o.timestamp != null ? o.timestamp
          : (o.lastUpdated != null ? o.lastUpdated
            : (o.dateCreated != null ? o.dateCreated : o.created)))
    );
  }
  function hitTime(h, ver) {
    var hits = (h && h.artifactHits) || [];
    for (var i = 0; i < hits.length; i++) {
      if (String(hits[i].version) === String(ver) && tsOf(hits[i])) return tsOf(hits[i]);
    }
    for (var j = 0; j < hits.length; j++) {
      var t = tsOf(hits[j]);
      if (t) return t;
    }
    return tsOf(h);
  }
  function fmtTs(ms) {
    var d = new Date(ms), p = function (n) { return (n < 10 ? '0' : '') + n; };
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
  }
  function jsonGet(url) {
    return fetch(url, {
      credentials: 'same-origin',
      headers: { 'Accept': 'application/json' }
    }).then(function (r) {
      if (r.status === 401 || r.status === 403) throw new Error('未登录(401/403)，请先在 Nexus 页面登录');
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    });
  }

  function searchOne(term) {
    var base = nexusBase(), url;
    var ci = term.indexOf(':');
    if (ci > 0) {
      url = base + '/service/local/lucene/search?g=' + encodeURIComponent(term.slice(0, ci).trim()) +
            '&a=' + encodeURIComponent(term.slice(ci + 1).trim());
    } else {
      url = base + '/service/local/lucene/search?q=' + encodeURIComponent(term);
    }
    // Nexus 2 默认回 XML（JSON.parse 报 Unexpected token '<'），必须显式要 JSON；
    // collapseresults=true 让服务端直接给每个构件的最新版，否则单构件 700+ 个版本全量返回。
    return jsonGet(url + '&collapseresults=true').then(function (j) { return (j && j.data) || []; });
  }

  function versionContentUrl(row) {
    var path = String(row.g || '').split('.').join('/') + '/' + row.a + '/' + row.ver + '/';
    return nexusBase() + '/service/local/repositories/' + encodeURIComponent(row.repo) + '/content/' + path;
  }

  // 与 Nexus 详情「Uploaded Date / Last Modified」同源：仓库 content 列表或 pom 元数据。
  function fetchArtifactTime(row) {
    if (!row.repo || !row.g || !row.a || !row.ver) return Promise.resolve(0);
    var dirUrl = versionContentUrl(row);
    var prefer = (row.a + '-' + row.ver).toLowerCase();

    function bestFromItems(items) {
      var best = 0, preferred = 0;
      (items || []).forEach(function (it) {
        var t = tsOf(it);
        if (!t) return;
        var name = String(it.text || it.name || it.leaf || '').toLowerCase();
        if (name.indexOf(prefer) === 0 || /\.(pom|jar|war)$/.test(name)) {
          if (t > preferred) preferred = t;
        }
        if (t > best) best = t;
      });
      return preferred || best;
    }

    return jsonGet(dirUrl).then(function (j) {
      var data = j && j.data;
      var items = Array.isArray(data) ? data : (data ? [data] : []);
      var t = bestFromItems(items);
      if (t) return t;
      // 目录无时间时，直接读主 pom 元数据
      return jsonGet(dirUrl + encodeURIComponent(row.a + '-' + row.ver + '.pom')).then(function (j2) {
        var d = j2 && j2.data;
        if (Array.isArray(d)) return bestFromItems(d);
        return tsOf(d);
      });
    }).catch(function () { return 0; });
  }

  function applyRowTime(row, ms) {
    if (!ms) return;
    row.ts = ms;
    if (row.tmEl) {
      row.tmEl.textContent = fmtTs(ms);
      row.tmEl.title = fmtTs(ms);
    }
  }

  function enrichMissingTimes(rows) {
    var need = (rows || []).filter(function (r) { return r && !r.ts && r.repo; });
    if (!need.length) return Promise.resolve();
    var i = 0, conc = 4;
    function worker() {
      if (i >= need.length) return Promise.resolve();
      var row = need[i++];
      return fetchArtifactTime(row).then(function (ms) {
        applyRowTime(row, ms);
        return worker();
      });
    }
    var jobs = [];
    for (var c = 0; c < conc && c < need.length; c++) jobs.push(worker());
    return Promise.all(jobs).then(function () {});
  }

  var CSS = [
    // 没有 border-box，textarea 的 width:100% + padding 会撑出 20px，面板底部就多出一条横向滚动条
    '#aiwb-nx-panel,#aiwb-nx-panel *{box-sizing:border-box;text-decoration:none!important;}',
    '#aiwb-nx-panel{position:fixed;top:14px;right:14px;z-index:2147483000;',
    'width:min(960px,calc(100vw - 28px));min-width:320px;max-height:min(88vh,920px);',
    'display:flex;flex-direction:column;',
    'background:linear-gradient(165deg,rgba(28,33,42,.985) 0%,rgba(17,20,26,.99) 100%);color:#e8eaed;',
    'border:1px solid rgba(255,255,255,.1);border-radius:14px;',
    'box-shadow:0 1px 0 rgba(255,255,255,.06) inset,0 18px 50px rgba(0,0,0,.55);',
    'font:12.5px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;overflow:hidden;',
    'backdrop-filter:blur(14px);-webkit-backdrop-filter:blur(14px);}',
    '#aiwb-nx-head{display:flex;align-items:center;gap:10px;padding:11px 14px;cursor:move;user-select:none;flex-shrink:0;',
    'background:linear-gradient(180deg,rgba(255,255,255,.06),rgba(255,255,255,.02));border-bottom:1px solid rgba(255,255,255,.07);}',
    '#aiwb-nx-badge{flex:0 0 auto;width:28px;height:28px;display:inline-flex;align-items:center;justify-content:center;',
    'background:linear-gradient(145deg,#14b8a6,#0f766e);color:#fff;border-radius:8px;font-weight:800;font-size:11px;',
    'letter-spacing:.4px;box-shadow:0 4px 12px rgba(13,148,136,.35);}',
    '#aiwb-nx-titles{display:flex;flex-direction:column;gap:1px;min-width:0;flex:1;}',
    '#aiwb-nx-titles b{font-size:13.5px;font-weight:650;letter-spacing:.2px;color:#f3f4f6;}',
    '#aiwb-nx-hint{color:#8b929e;font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}',
    '#aiwb-nx-head button{background:none;border:0;color:#9ca3af;cursor:pointer;font-size:14px;padding:0;border-radius:6px;',
    'width:26px;height:26px;line-height:26px;text-align:center;flex-shrink:0;}',
    '#aiwb-nx-head button:hover{color:#fff;background:rgba(255,255,255,.08);}',
    '#aiwb-nx-close:hover{background:rgba(248,113,113,.2)!important;color:#fca5a5!important;}',
    '#aiwb-nx-body{padding:12px 14px 14px;display:flex;flex-direction:column;gap:10px;overflow:auto;min-height:0;flex:1;}',
    '#aiwb-nx-input{width:100%;min-width:0;height:92px;max-height:40vh;background:rgba(8,10,14,.88);color:#d8dde6;',
    'border:1px solid rgba(255,255,255,.1);border-radius:10px;padding:10px 12px;resize:vertical;',
    'font:12.5px/1.55 Consolas,"Cascadia Mono",ui-monospace,monospace;transition:border-color .15s,box-shadow .15s;}',
    '#aiwb-nx-input:focus{outline:none;border-color:rgba(20,184,166,.65);box-shadow:0 0 0 3px rgba(13,148,136,.2);}',
    '#aiwb-nx-input::placeholder{color:#5b6472;}',
    '#aiwb-nx-btns{display:flex;align-items:center;gap:8px;flex-wrap:wrap;}',
    '#aiwb-nx-btns button{background:rgba(255,255,255,.05);color:#d1d5db;border:1px solid rgba(255,255,255,.1);',
    'border-radius:8px;padding:6px 12px;cursor:pointer;font:inherit;font-size:12px;',
    'transition:background .15s,color .15s,border-color .15s,transform .1s;}',
    '#aiwb-nx-btns button:hover{background:rgba(255,255,255,.1);color:#fff;border-color:rgba(255,255,255,.18);}',
    '#aiwb-nx-btns button:active{transform:translateY(1px);}',
    '#aiwb-nx-btns button.pri{background:linear-gradient(180deg,#14b8a6,#0d9488);border-color:transparent;color:#fff;font-weight:650;',
    'box-shadow:0 4px 14px rgba(13,148,136,.3);}',
    '#aiwb-nx-btns button.pri:hover{background:linear-gradient(180deg,#2dd4bf,#0f766e);}',
    '#aiwb-nx-btns button:disabled{opacity:.45;cursor:not-allowed;transform:none;box-shadow:none;}',
    '#aiwb-nx-status{margin-left:auto;max-width:100%;min-height:22px;padding:3px 10px;border-radius:999px;font-size:11.5px;',
    'color:#94a3b8;background:rgba(255,255,255,.04);border:1px solid transparent;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}',
    '#aiwb-nx-status:empty{display:none;}',
    '#aiwb-nx-status.done{color:#6ee7b7;background:rgba(16,185,129,.1);border-color:rgba(16,185,129,.22);}',
    '#aiwb-nx-status.err{color:#fca5a5;background:rgba(248,113,113,.1);border-color:rgba(248,113,113,.22);}',
    // min-width:0 必需：body 是 flex 列，子项默认 min-width:auto 会被表格 nowrap 宽度撑出去
    '#aiwb-nx-wrap{overflow:auto;min-width:0;flex:1 1 auto;border:1px solid rgba(255,255,255,.08);border-radius:10px;',
    'background:rgba(0,0,0,.2);overscroll-behavior:contain;}',
    '#aiwb-nx-panel,#aiwb-nx-panel *{scrollbar-width:thin;scrollbar-color:rgba(255,255,255,.18) transparent;}',
    '#aiwb-nx-panel ::-webkit-scrollbar,#aiwb-nx-panel *::-webkit-scrollbar{width:8px;height:8px;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-track,#aiwb-nx-panel *::-webkit-scrollbar-track{background:transparent;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-thumb,#aiwb-nx-panel *::-webkit-scrollbar-thumb{background:rgba(255,255,255,.14);border:2px solid transparent;border-radius:8px;background-clip:padding-box;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-thumb:hover,#aiwb-nx-panel *::-webkit-scrollbar-thumb:hover{background:rgba(255,255,255,.28);background-clip:padding-box;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-corner,#aiwb-nx-panel *::-webkit-scrollbar-corner{background:transparent;}',
    '#aiwb-nx-tbl{width:100%;border-collapse:separate;border-spacing:0;font-family:Consolas,"Cascadia Mono",ui-monospace,monospace;font-size:12px;}',
    '#aiwb-nx-tbl th{position:sticky;top:0;z-index:1;background:rgba(24,30,40,.97);padding:9px 10px;text-align:left;',
    'font-size:10px;font-weight:650;color:#8b949e;letter-spacing:.4px;text-transform:uppercase;',
    'white-space:nowrap;box-shadow:inset 0 -1px 0 rgba(255,255,255,.07);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;}',
    '#aiwb-nx-tbl td{padding:7px 10px;border-top:1px solid rgba(255,255,255,.05);white-space:nowrap;vertical-align:middle;color:#d1d5db;}',
    '#aiwb-nx-tbl tbody tr:nth-child(even) td{background:rgba(255,255,255,.018);}',
    '#aiwb-nx-tbl tbody tr:hover td{background:rgba(20,184,166,.07);}',
    '#aiwb-nx-tbl tr.empty td{color:#7b8494;text-align:left;padding:32px 18px;font-size:12.5px;border-top:0;white-space:normal;',
    'line-height:1.75;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;}',
    '#aiwb-nx-tbl tr.empty:hover td{background:transparent;}',
    '#aiwb-nx-tbl tr.empty b{color:#c4cad4;font-weight:650;}',
    '#aiwb-nx-tbl tr.empty .ic{display:inline-flex;width:22px;height:22px;align-items:center;justify-content:center;',
    'margin-right:8px;border-radius:6px;background:rgba(255,255,255,.06);vertical-align:-4px;opacity:.9;}',
    '#aiwb-nx-tbl tr.empty kbd{background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.12);border-bottom-width:2px;',
    'border-radius:5px;padding:1px 6px;font:11.5px Consolas,monospace;color:#b6bec9;}',
    '#aiwb-nx-tbl td.num{color:#6b7280;font-variant-numeric:tabular-nums;width:2em;text-align:right;padding-right:6px;}',
    '#aiwb-nx-tbl td.g{color:#9aa3b2;max-width:160px;overflow:hidden;text-overflow:ellipsis;}',
    '#aiwb-nx-tbl td.a{color:#e5e7eb;font-weight:600;max-width:220px;overflow:hidden;text-overflow:ellipsis;}',
    '#aiwb-nx-tbl td.tm{color:#9aa3b2;font-variant-numeric:tabular-nums;font-size:11.5px;}',
    '#aiwb-nx-tbl td.ver,#aiwb-nx-tbl td.up{cursor:pointer;padding-top:5px;padding-bottom:5px;}',
    '#aiwb-nx-tbl .chip{display:inline-block;padding:2px 8px;border-radius:6px;font-weight:650;font-size:11.5px;',
    'line-height:1.45;max-width:200px;overflow:hidden;text-overflow:ellipsis;vertical-align:middle;',
    'transition:background .12s,box-shadow .12s,color .12s;}',
    '#aiwb-nx-tbl .chip-ver{color:#6ee7b7;background:rgba(16,185,129,.12);border:1px solid rgba(16,185,129,.22);}',
    '#aiwb-nx-tbl .chip-up{color:#93c5fd;background:rgba(59,130,246,.12);border:1px solid rgba(59,130,246,.22);}',
    '#aiwb-nx-tbl td.ver:hover .chip-ver{background:rgba(16,185,129,.22);box-shadow:0 0 0 1px rgba(16,185,129,.35);}',
    '#aiwb-nx-tbl td.up:hover .chip-up{background:rgba(59,130,246,.22);box-shadow:0 0 0 1px rgba(59,130,246,.35);}',
    '#aiwb-nx-tbl .chip.flash{color:#fff!important;background:rgba(255,255,255,.2)!important;border-color:rgba(255,255,255,.35)!important;}',
    '#aiwb-nx-tbl td.repo{font-size:11.5px;}',
    '#aiwb-nx-tbl .repo-tag{color:#cbd5e1;}',
    '#aiwb-nx-tbl .hit{margin-left:6px;padding:1px 6px;border-radius:999px;font-size:10px;font-weight:650;',
    'color:#94a3b8;background:rgba(255,255,255,.06);border:1px solid rgba(255,255,255,.08);}',
    '#aiwb-nx-tbl td.pend{color:#7b8494;}',
    '#aiwb-nx-tbl td.pend.spin{color:#2dd4bf;}',
    '#aiwb-nx-tbl tr.err td{color:#fca5a5;}',
    '#aiwb-nx-tbl .rowbtn{background:rgba(255,255,255,.04);border:1px solid rgba(255,255,255,.12);color:#b6bec9;',
    'border-radius:6px;cursor:pointer;padding:3px 10px;font:11.5px -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;',
    'transition:background .12s,color .12s,border-color .12s;}',
    '#aiwb-nx-tbl .rowbtn:hover{color:#fff;border-color:rgba(255,255,255,.28);background:rgba(255,255,255,.08);}',
    '#aiwb-nx-resize{position:absolute;right:0;bottom:0;width:16px;height:16px;cursor:nwse-resize;z-index:3;',
    'background:linear-gradient(135deg,transparent 55%,rgba(255,255,255,.22) 55%,rgba(255,255,255,.22) 70%,transparent 70%,transparent 80%,rgba(255,255,255,.18) 80%);',
    'border-radius:0 0 14px 0;}',
    '#aiwb-nx-panel.min{width:auto!important;min-width:200px;height:auto!important;}',
    '#aiwb-nx-panel.min #aiwb-nx-body,#aiwb-nx-panel.min #aiwb-nx-resize{display:none;}',
    '#aiwb-nx-panel.min #aiwb-nx-hint{display:none;}',
    '@media (max-width:640px){',
    '#aiwb-nx-panel{top:8px;right:8px;left:8px;width:auto;border-radius:12px;}',
    '#aiwb-nx-hint{display:none;}',
    '#aiwb-nx-input{height:72px;}',
    '#aiwb-nx-status{margin-left:0;flex:1 1 100%;text-align:left;}',
    '#aiwb-nx-tbl td.g{max-width:96px;}',
    '#aiwb-nx-tbl td.a{max-width:120px;}',
    '#aiwb-nx-tbl .chip{max-width:120px;}',
    '}',
    '@keyframes aiwb-nx-blink{0%,100%{opacity:.35}50%{opacity:1}}',
    '#aiwb-nx-tbl td.spin{animation:aiwb-nx-blink 1.1s ease-in-out infinite;}'
  ].join('');

  var panel, tblBody, statusEl, inputEl, goBtn, running = false;
  var slots = [];   // 本轮查询的行槽位：{ term, tr, rows: [] , err }
  var COLS = 8;
  var EMPTY_HTML = '<tr class="empty"><td colspan="' + COLS + '"><span class="ic">⌕</span>还没有查询结果 · 每行粘贴一个构件名（或 <b>group:artifact</b>），点 <b>批量查询</b> 或按 <kbd>Ctrl</kbd>+<kbd>Enter</kbd></td></tr>';

  function setStatus(msg, cls) {
    statusEl.textContent = msg;
    statusEl.className = cls || '';
  }

  function renumber() {
    var rows = tblBody.querySelectorAll('tr');
    for (var i = 0; i < rows.length; i++) {
      var first = rows[i].querySelector('td');
      if (first && first.classList.contains('num')) first.textContent = i + 1;
    }
  }

  function allResultRows() {
    var out = [];
    slots.forEach(function (s) { out = out.concat(s.rows || []); });
    return out;
  }

  function buildPanel() {
    var style = document.createElement('style');
    style.textContent = CSS;
    document.head.appendChild(style);

    panel = document.createElement('div');
    panel.id = 'aiwb-nx-panel';
    panel.innerHTML =
      '<div id="aiwb-nx-head"><span id="aiwb-nx-badge">NX</span>' +
      '<div id="aiwb-nx-titles"><b>Nexus 批量查询</b>' +
      '<span id="aiwb-nx-hint">每行一个构件名 / group:artifact · Ctrl+Enter</span></div>' +
      '<button id="aiwb-nx-min" title="最小化">—</button>' +
      '<button id="aiwb-nx-close" title="关闭">×</button></div>' +
      '<div id="aiwb-nx-body"><textarea id="aiwb-nx-input" spellcheck="false" placeholder="demo.shop.parent.pom&#10;com.example.shop:demo.shop.account.sdk"></textarea>' +
      '<div id="aiwb-nx-btns"><button class="pri" id="aiwb-nx-go">批量查询</button>' +
      '<button id="aiwb-nx-copy" title="复制全部为 TSV">复制全部</button>' +
      '<button id="aiwb-nx-copy-up" title="复制 artifactId + 升级版本">复制升级版本</button>' +
      '<span id="aiwb-nx-status"></span></div>' +
      '<div id="aiwb-nx-wrap"><table id="aiwb-nx-tbl"><thead><tr>' +
      '<th>#</th><th>groupId</th><th>artifactId</th><th>最新版本</th><th>发布</th><th>升级版本</th><th>仓库</th><th></th>' +
      '</tr></thead><tbody></tbody></table></div></div>' +
      '<div id="aiwb-nx-resize" title="拖动调整大小"></div>';
    document.body.appendChild(panel);

    tblBody = panel.querySelector('tbody');
    tblBody.innerHTML = EMPTY_HTML;
    statusEl = panel.querySelector('#aiwb-nx-status');
    inputEl = panel.querySelector('#aiwb-nx-input');
    goBtn = panel.querySelector('#aiwb-nx-go');

    var st = loadState();
    if (st.input) inputEl.value = st.input;
    if (typeof st.w === 'number' && st.w >= 320) panel.style.width = Math.min(st.w, window.innerWidth - 28) + 'px';
    if (typeof st.h === 'number' && st.h >= 280) panel.style.height = Math.min(st.h, window.innerHeight - 28) + 'px';
    if (typeof st.x === 'number' && typeof st.y === 'number') {
      panel.style.right = 'auto';
      panel.style.left = Math.min(st.x, window.innerWidth - 80) + 'px';
      panel.style.top = Math.min(st.y, window.innerHeight - 40) + 'px';
    }

    panel.querySelector('#aiwb-nx-close').onclick = function () { panel.remove(); };
    var minBtn = panel.querySelector('#aiwb-nx-min');
    minBtn.onclick = function () {
      var min = panel.classList.toggle('min');
      minBtn.textContent = min ? '▢' : '—';
      minBtn.title = min ? '还原' : '最小化';
      if (min) panel.title = '点击标题栏拖动'; else panel.title = '';
    };
    goBtn.onclick = runBatch;
    panel.querySelector('#aiwb-nx-copy').onclick = copyAll;
    panel.querySelector('#aiwb-nx-copy-up').onclick = copyUpgrade;
    inputEl.addEventListener('input', function () { saveState({ input: inputEl.value }); });
    inputEl.addEventListener('keydown', function (e) {
      if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); runBatch(); }
    });

    // 标题栏拖动（带位置记忆）
    var head = panel.querySelector('#aiwb-nx-head');
    head.addEventListener('pointerdown', function (e) {
      if (e.target.tagName === 'BUTTON') return;
      var sx = e.clientX, sy = e.clientY;
      var r = panel.getBoundingClientRect(), ol = r.left, ot = r.top;
      panel.style.right = 'auto'; panel.style.left = ol + 'px'; panel.style.top = ot + 'px';
      function mv(ev) {
        var nx = Math.max(0, Math.min(ol + ev.clientX - sx, window.innerWidth - 80));
        var ny = Math.max(0, Math.min(ot + ev.clientY - sy, window.innerHeight - 40));
        panel.style.left = nx + 'px'; panel.style.top = ny + 'px';
      }
      function up() {
        document.removeEventListener('pointermove', mv);
        document.removeEventListener('pointerup', up);
        var r2 = panel.getBoundingClientRect();
        saveState({ x: Math.round(r2.left), y: Math.round(r2.top) });
      }
      document.addEventListener('pointermove', mv);
      document.addEventListener('pointerup', up);
    });

    // 右下角拖动改宽高
    var grip = panel.querySelector('#aiwb-nx-resize');
    grip.addEventListener('pointerdown', function (e) {
      e.preventDefault(); e.stopPropagation();
      var sx = e.clientX, sy = e.clientY;
      var r = panel.getBoundingClientRect(), ow = r.width, oh = r.height;
      panel.style.right = 'auto'; panel.style.left = r.left + 'px'; panel.style.top = r.top + 'px';
      function mv(ev) {
        var nw = Math.max(320, Math.min(ow + ev.clientX - sx, window.innerWidth - r.left - 8));
        var nh = Math.max(280, Math.min(oh + ev.clientY - sy, window.innerHeight - r.top - 8));
        panel.style.width = nw + 'px';
        panel.style.height = nh + 'px';
      }
      function up() {
        document.removeEventListener('pointermove', mv);
        document.removeEventListener('pointerup', up);
        saveState({ w: Math.round(panel.getBoundingClientRect().width), h: Math.round(panel.getBoundingClientRect().height) });
      }
      document.addEventListener('pointermove', mv);
      document.addEventListener('pointerup', up);
    });
  }

  function pendingRow(term) {
    var tr = document.createElement('tr');
    tr.innerHTML = '<td class="num"></td><td class="pend spin" colspan="' + (COLS - 1) + '">查询中 · ' + esc(term) + '</td>';
    tblBody.appendChild(tr);
    return tr;
  }

  function openBtn(row) {
    var b = document.createElement('button');
    b.className = 'rowbtn';
    b.textContent = '打开';
    b.title = '在系统浏览器打开该版本目录';
    b.onclick = function () {
      // aiwb-shell.open 魔法主机：AI Workbench 注入窗会把链接转交系统浏览器
      var a = document.createElement('a');
      a.href = 'https://aiwb-shell.open/?url=' + encodeURIComponent(
        nexusBase() + '/content/repositories/' + row.repo + '/' + row.g.split('.').join('/') + '/' + row.a + '/' + row.ver + '/');
      document.body.appendChild(a); a.click(); a.remove();
    };
    return b;
  }

  function retryBtn(slot) {
    var b = document.createElement('button');
    b.className = 'rowbtn';
    b.textContent = '重试';
    b.onclick = function () { querySlot(slot); };
    return b;
  }

  function setSlotPending(slot) {
    slot.tr.className = '';
    slot.tr.innerHTML = '<td class="num"></td><td class="pend spin" colspan="' + (COLS - 1) + '">查询中 · ' + esc(slot.term) + '</td>';
    renumber();
  }

  function setSlotError(slot, errMsg) {
    slot.rows = []; slot.err = errMsg;
    var tr = slot.tr;
    tr.className = 'err';
    tr.innerHTML = '<td class="num"></td><td>—</td><td class="a">' + esc(slot.term) + '</td>' +
      '<td colspan="4">' + esc(errMsg || '查询失败') + '</td><td></td>';
    var act = tr.lastChild;
    act.appendChild(retryBtn(slot));
    renumber();
  }

  function setSlotResult(slot, hits) {
    slot.err = null;
    // 按 group:artifact 聚合取最新版本
    var map = {};
    hits.forEach(function (h) {
      var k = (h.groupId || '') + ':' + (h.artifactId || '');
      var v = h.latestRelease || h.version || '';
      if (!map[k] || cmpVersion(v, map[k].ver) > 0) {
        var repo = h.repoId || h.latestReleaseRepositoryId ||
          (h.artifactHits && h.artifactHits[0] && h.artifactHits[0].repositoryId) || '';
        map[k] = { g: h.groupId, a: h.artifactId, ver: v, repo: repo, ts: hitTime(h, v), n: 0 };
      }
      map[k].n++;
    });

    var keys = Object.keys(map).sort();
    slot.rows = keys.map(function (k) { return map[k]; });

    if (!keys.length) {
      slot.tr.className = '';
      slot.tr.innerHTML = '<td class="num"></td><td>—</td><td class="a">' + esc(slot.term) + '</td>' +
        '<td colspan="4" class="pend">无结果</td><td></td>';
    } else {
      var first = slot.tr;
      keys.forEach(function (k, i) {
        var row = map[k];
        var up = bumpVersion(row.ver);
        var tr = i === 0 ? first : document.createElement('tr');
        tr.className = '';
        tr.innerHTML =
          '<td class="num"></td>' +
          '<td class="g" title="' + esc(row.g) + '">' + esc(row.g) + '</td>' +
          '<td class="a" title="' + esc(row.a) + '">' + esc(row.a) + '</td>' +
          '<td class="ver" title="点击复制 ' + esc(row.g + ':' + row.a + ':' + row.ver) + '"><span class="chip chip-ver">' + esc(row.ver) + '</span></td>' +
          '<td class="tm" title="' + (row.ts ? esc(fmtTs(row.ts)) : '补查上传时间…') + '">' + (row.ts ? esc(fmtTs(row.ts)) : '…') + '</td>' +
          '<td class="up" title="点击复制 ' + esc(row.a + '\t' + up) + '"><span class="chip chip-up">' + esc(up) + '</span></td>' +
          '<td class="repo"><span class="repo-tag">' + esc(row.repo) + '</span><span class="hit" title="命中数">×' + row.n + '</span></td>' +
          '<td></td>';
        var act = tr.lastChild;
        act.appendChild(openBtn(row));
        row.tmEl = tr.querySelector('.tm');
        var verTd = tr.querySelector('.ver'), upTd = tr.querySelector('.up');
        verTd.onclick = function () { copyText(row.g + ':' + row.a + ':' + row.ver); flashChip(verTd); };
        upTd.onclick = function () { copyText(row.a + '\t' + up); flashChip(upTd); };
        if (i !== 0) first.parentNode.insertBefore(tr, first.nextSibling);
      });
    }
    renumber();
  }

  function flashChip(td) {
    var chip = td.querySelector('.chip');
    if (!chip) return;
    chip.classList.add('flash');
    setTimeout(function () { chip.classList.remove('flash'); }, 280);
  }

  function querySlot(slot) {
    setSlotPending(slot);
    return searchOne(slot.term).then(function (hits) {
      setSlotResult(slot, hits);
      return enrichMissingTimes(slot.rows).then(function () {
        (slot.rows || []).forEach(function (r) {
          if (!r.ts && r.tmEl) {
            r.tmEl.textContent = '—';
            r.tmEl.title = '未取到上传时间';
          }
        });
      });
    }).catch(function (e) {
      setSlotError(slot, e.message);
    });
  }

  function parseTerms() {
    return inputEl.value.split('\n')
      .map(function (s) { return s.trim(); })
      .filter(function (s, i, arr) { return s && arr.indexOf(s) === i; });
  }

  function runBatch() {
    if (running) return;
    var terms = parseTerms();
    if (!terms.length) { setStatus('请先粘贴要查询的构件名', 'err'); return; }

    running = true;
    goBtn.disabled = true;
    tblBody.innerHTML = '';
    slots = terms.map(function (term) {
      var slot = { term: term, rows: [], err: null, tr: null };
      slot.tr = pendingRow(term);
      return slot;
    });
    renumber();
    setStatus('查询中 0/' + terms.length);

    var done = 0;
    function next(idx) {
      if (idx >= terms.length) {
        running = false;
        goBtn.disabled = false;
        var ok = slots.filter(function (s) { return !s.err && s.rows.length; }).length;
        var bad = slots.length - ok;
        setStatus('完成 ' + done + '/' + terms.length + ' · 成功 ' + ok + (bad ? ' · 失败 ' + bad + '（可单行重试）' : ''), bad ? 'err' : 'done');
        return;
      }
      querySlot(slots[idx]).then(function () {
        done++;
        setStatus('查询中 ' + done + '/' + terms.length);
        next(idx + 4);
      });
    }
    for (var i = 0; i < 4 && i < terms.length; i++) next(i);  // 并发 4
  }

  function copyText(s) {
    if (navigator.clipboard && navigator.clipboard.writeText) { navigator.clipboard.writeText(s); return; }
    var t = document.createElement('textarea'); t.value = s; document.body.appendChild(t);
    t.select(); try { document.execCommand('copy'); } catch (e) {} t.remove();
  }

  function copyAll() {
    var rows = allResultRows();
    if (!rows.length) { setStatus('没有可复制的结果', 'err'); return; }
    var tsv = ['groupId\tartifactId\t最新版本\t发布时间\t升级版本\t仓库\t命中数'].concat(rows.map(function (r) {
      return r.g + '\t' + r.a + '\t' + r.ver + '\t' + (r.ts ? fmtTs(r.ts) : '') + '\t' + bumpVersion(r.ver) + '\t' + r.repo + '\t' + r.n;
    })).join('\n');
    copyText(tsv);
    setStatus('已复制 ' + rows.length + ' 行 TSV', 'done');
  }

  function copyUpgrade() {
    var rows = allResultRows();
    if (!rows.length) { setStatus('没有可复制的结果', 'err'); return; }
    var tsv = rows.map(function (r) {
      return r.a + '\t' + bumpVersion(r.ver);
    }).join('\n');
    copyText(tsv);
    setStatus('已复制 ' + rows.length + ' 行 artifactId + 升级版本', 'done');
  }

  function boot() {
    if (!document.body) { setTimeout(boot, 200); return; }
    buildPanel();
  }
  boot();
})();
