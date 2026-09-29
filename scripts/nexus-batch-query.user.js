/**
 * Nexus 2 批量查询服务（AI Workbench 网页工具 · 用户脚本）
 *
 * 用法：网页工具页 → 脚本 → 新建脚本
 *   名称：Nexus 批量查询服务
 *   匹配规则：填你的 Nexus 地址，如  http://10.0.0.1/nexus/*  （也可 all_urls，脚本会自检不在 Nexus 页面就不激活）
 *   代码：粘贴本文件全文
 *
 * 功能：在 Nexus 2 页面右上角注入悬浮面板，粘贴多行构件名批量查询最新 RELEASE 版本。
 *   - 每行一个关键词（如 pmys.saas.parent.pom），或 group:artifact 精确查（如 com.pmys.saas:pmys.saas.account.sdk）
 *   - 走 Nexus 自带 REST：/service/local/lucene/search，登录态自动带上（同源 fetch）
 *   - 结果按输入顺序呈现，查询中/失败/无结果三态分明，失败行可单行重试；未查询时列表区显示引导空态
 *   - 最新版本点击复制 g:a:v；发布时间取该版本在 Nexus 的上传时间戳；升级版本（末段+1，满 999 向前进位）点击复制 artifactId+新版本
 *   - 一键复制全部（TSV）/ 一键复制 artifactId+升级版本（TSV）
 *   - 并发 4，Ctrl+Enter 直接查询；输入内容与面板位置自动记忆（localStorage）
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

  // 最新版本生成时间：Nexus 2 的 lucene 结果里时间字段命名不统一，且 collapseresults 后
  // 顶层 timestamp 未必对应我们选中的那个版本，所以优先按版本号在 artifactHits 里找。
  function tsOf(o) {
    if (!o) return 0;
    var t = o.timestamp != null ? o.timestamp : (o.lastUpdated != null ? o.lastUpdated : o.dateCreated);
    var n = Number(t);
    return n > 0 ? n : 0;
  }
  function hitTime(h, ver) {
    var hits = (h && h.artifactHits) || [];
    for (var i = 0; i < hits.length; i++) {
      if (String(hits[i].version) === String(ver) && tsOf(hits[i])) return tsOf(hits[i]);
    }
    return tsOf(h);
  }
  function fmtTs(ms) {
    var d = new Date(ms), p = function (n) { return (n < 10 ? '0' : '') + n; };
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
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
    return fetch(url + '&collapseresults=true', {
      credentials: 'same-origin',
      headers: { 'Accept': 'application/json' }
    }).then(function (r) {
      if (r.status === 401 || r.status === 403) throw new Error('未登录(401/403)，请先在 Nexus 页面登录');
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    }).then(function (j) { return (j && j.data) || []; });
  }

  var CSS = [
    // 没有 border-box，textarea 的 width:100% + padding 会撑出 20px，面板底部就多出一条横向滚动条
    '#aiwb-nx-panel,#aiwb-nx-panel *{box-sizing:border-box;}',
    '#aiwb-nx-panel{position:fixed;top:14px;right:14px;z-index:2147483000;width:860px;max-height:88vh;display:flex;flex-direction:column;',
    'background:linear-gradient(180deg,#161b22,#11151b);color:#e5e7eb;border:1px solid rgba(255,255,255,.12);border-radius:12px;',
    'box-shadow:0 16px 48px rgba(0,0,0,.55);font:12.5px/1.55 Consolas,monospace;overflow:hidden;}',
    '#aiwb-nx-head{display:flex;align-items:center;gap:8px;padding:9px 12px;cursor:move;user-select:none;',
    'background:rgba(255,255,255,.04);border-bottom:1px solid rgba(255,255,255,.08);}',
    '#aiwb-nx-badge{background:linear-gradient(135deg,#10b981,#0d9488);color:#fff;border-radius:6px;padding:2px 7px;font-weight:700;font-size:11px;letter-spacing:.5px;}',
    '#aiwb-nx-head b{font-size:13px;}',
    '#aiwb-nx-hint{color:#6b7280;font-size:11px;}',
    '#aiwb-nx-head .sp{flex:1;}',
    '#aiwb-nx-head button{background:none;border:0;color:#9ca3af;cursor:pointer;font-size:14px;padding:0 2px;border-radius:4px;}',
    '#aiwb-nx-head button:hover{color:#fff;}',
    '#aiwb-nx-min{width:20px;height:20px;line-height:20px;border-radius:5px !important;text-align:center;padding:0 !important;font-weight:700;}',
    '#aiwb-nx-min:hover{background:rgba(255,255,255,.12);}',
    '#aiwb-nx-close:hover{background:rgba(248,113,113,.18);color:#f87171 !important;}',
    '#aiwb-nx-body{padding:10px 12px 12px;display:flex;flex-direction:column;gap:8px;overflow:auto;}',
    '#aiwb-nx-input{width:100%;min-width:0;height:110px;background:#0b0e12;color:#d1d5db;border:1px solid rgba(255,255,255,.14);border-radius:8px;padding:7px 9px;resize:vertical;',
    'font:12px/1.6 Consolas,monospace;transition:border-color .15s,box-shadow .15s;}',
    '#aiwb-nx-input:focus{outline:none;border-color:#0d9488;box-shadow:0 0 0 2px rgba(13,148,136,.25);}',
    '#aiwb-nx-input::placeholder{color:#4b5563;}',
    '#aiwb-nx-btns{display:flex;align-items:center;gap:6px;flex-wrap:wrap;}',
    '#aiwb-nx-btns button{background:#1f2937;color:#d1d5db;border:1px solid rgba(255,255,255,.14);border-radius:7px;padding:5px 13px;cursor:pointer;',
    'transition:background .15s,color .15s,border-color .15s;}',
    '#aiwb-nx-btns button:hover{background:#374151;color:#fff;}',
    '#aiwb-nx-btns button.pri{background:#0d9488;border-color:#0d9488;color:#fff;font-weight:600;}',
    '#aiwb-nx-btns button.pri:hover{background:#0f766e;}',
    '#aiwb-nx-btns button:disabled{opacity:.5;cursor:not-allowed;}',
    '#aiwb-nx-status{color:#94a3b8;margin-left:auto;min-height:16px;}',
    '#aiwb-nx-status.done{color:#34d399;}',
    '#aiwb-nx-status.err{color:#f87171;}',
    // min-width:0 必需：body 是 flex 列，子项默认 min-width:auto 会被表格 nowrap 宽度撑出去，导致面板底部多出一条横向滚动条
    '#aiwb-nx-wrap{overflow:auto;min-width:0;border:1px solid rgba(255,255,255,.08);border-radius:8px;overscroll-behavior:contain;}',
    // 细滚动条：Windows 原生亮色粗条在暗面板上很突兀（textarea 与结果表两处）
    '#aiwb-nx-panel,#aiwb-nx-panel *{scrollbar-width:thin;scrollbar-color:rgba(255,255,255,.18) transparent;}',
    '#aiwb-nx-panel ::-webkit-scrollbar,#aiwb-nx-panel *::-webkit-scrollbar{width:10px;height:10px;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-track,#aiwb-nx-panel *::-webkit-scrollbar-track{background:transparent;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-thumb,#aiwb-nx-panel *::-webkit-scrollbar-thumb{background:rgba(255,255,255,.16);border:3px solid transparent;border-radius:8px;background-clip:padding-box;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-thumb:hover,#aiwb-nx-panel *::-webkit-scrollbar-thumb:hover{background:rgba(255,255,255,.3);background-clip:padding-box;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-thumb:active,#aiwb-nx-panel *::-webkit-scrollbar-thumb:active{background:rgba(255,255,255,.42);background-clip:padding-box;}',
    '#aiwb-nx-panel ::-webkit-scrollbar-corner,#aiwb-nx-panel *::-webkit-scrollbar-corner{background:transparent;}',
    '#aiwb-nx-tbl{width:100%;border-collapse:collapse;}',
    '#aiwb-nx-tbl th{position:sticky;top:0;z-index:1;background:#1a212b;padding:6px 10px;text-align:left;font-size:11px;font-weight:600;color:#9ca3af;letter-spacing:.3px;white-space:nowrap;box-shadow:inset 0 -1px 0 rgba(255,255,255,.08);}',
    '#aiwb-nx-tbl td{padding:5px 10px;border-top:1px solid rgba(255,255,255,.06);white-space:nowrap;}',
    '#aiwb-nx-tbl tbody tr:hover td{background:rgba(255,255,255,.03);}',
    // 空态：未查询时给出引导，否则只剩表头，像样式坏掉。
    // 必须左对齐：8 个 nowrap 表头让表格 min-content 宽于面板，居中会把提示推到横向滚动区之外
    '#aiwb-nx-tbl tr.empty td{color:#6b7280;text-align:left;padding:24px 12px;font-size:12px;border-top:0;white-space:normal;line-height:1.8;}',
    '#aiwb-nx-tbl tr.empty:hover td{background:transparent;}',
    '#aiwb-nx-tbl tr.empty b{color:#9ca3af;font-weight:700;}',
    '#aiwb-nx-tbl tr.empty .ic{margin-right:8px;opacity:.75;}',
    '#aiwb-nx-tbl tr.empty kbd{background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.14);border-bottom-width:2px;border-radius:4px;padding:0 4px;font:inherit;color:#9ca3af;}',
    '#aiwb-nx-tbl td.num{color:#6b7280;}',
    '#aiwb-nx-tbl td.tm{color:#9ca3af;font-variant-numeric:tabular-nums;white-space:nowrap;}',
    '#aiwb-nx-tbl td.ver{color:#34d399;font-weight:700;cursor:pointer;}',
    '#aiwb-nx-tbl td.up{color:#60a5fa;font-weight:700;cursor:pointer;}',
    '#aiwb-nx-tbl td.ver:hover,#aiwb-nx-tbl td.up:hover{text-decoration:underline;text-underline-offset:3px;}',
    '#aiwb-nx-tbl td.pend{color:#6b7280;}',
    '#aiwb-nx-tbl td.pend.spin{color:#0d9488;}',
    '#aiwb-nx-tbl tr.err td{color:#f87171;}',
    '#aiwb-nx-tbl .rowbtn{background:transparent;border:1px solid rgba(255,255,255,.2);color:#9ca3af;border-radius:5px;cursor:pointer;padding:1px 8px;font:11.5px Consolas,monospace;}',
    '#aiwb-nx-tbl .rowbtn:hover{color:#fff;border-color:#fff;background:rgba(255,255,255,.06);}',
    '#aiwb-nx-panel.min{width:auto;}',
    '#aiwb-nx-panel.min #aiwb-nx-body{display:none;}',
    '#aiwb-nx-panel.min #aiwb-nx-hint{display:none;}',
    '@keyframes aiwb-nx-blink{0%,100%{opacity:.35}50%{opacity:1}}',
    '#aiwb-nx-tbl td.spin{animation:aiwb-nx-blink 1.1s ease-in-out infinite;}'
  ].join('');

  var panel, tblBody, statusEl, inputEl, goBtn, running = false;
  var slots = [];   // 本轮查询的行槽位：{ term, tr, rows: [] , err }
  var EMPTY_HTML = '<tr class="empty"><td colspan="9"><span class="ic">&#128269;</span>还没有查询结果 · 每行粘贴一个构件名（或 <b>group:artifact</b>），点 <b>批量查询</b> 或按 <kbd>Ctrl</kbd>+<kbd>Enter</kbd></td></tr>';

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
      '<div id="aiwb-nx-head"><span id="aiwb-nx-badge">NX</span><b>Nexus 批量查询</b>' +
      '<span id="aiwb-nx-hint">每行一个构件名 / group:artifact · Ctrl+Enter 查询</span>' +
      '<span class="sp"></span>' +
      '<button id="aiwb-nx-min" title="最小化">—</button>' +
      '<button id="aiwb-nx-close" title="关闭">×</button></div>' +
      '<div id="aiwb-nx-body"><textarea id="aiwb-nx-input" placeholder="pmys.saas.parent.pom\npmys.saas.account.sdk"></textarea>' +
      '<div id="aiwb-nx-btns"><button class="pri" id="aiwb-nx-go">批量查询</button>' +
      '<button id="aiwb-nx-copy">复制全部 (TSV)</button>' +
      '<button id="aiwb-nx-copy-up">复制升级版本 (TSV)</button>' +
      '<span id="aiwb-nx-status"></span></div>' +
      '<div id="aiwb-nx-wrap"><table id="aiwb-nx-tbl"><thead><tr>' +
      '<th>#</th><th>groupId</th><th>artifactId</th><th>最新版本</th><th>发布时间</th><th>升级版本</th><th>仓库</th><th>命中</th><th>操作</th>' +
      '</tr></thead><tbody></tbody></table></div></div>';
    document.body.appendChild(panel);

    tblBody = panel.querySelector('tbody');
    tblBody.innerHTML = EMPTY_HTML;
    statusEl = panel.querySelector('#aiwb-nx-status');
    inputEl = panel.querySelector('#aiwb-nx-input');
    goBtn = panel.querySelector('#aiwb-nx-go');

    var st = loadState();
    if (st.input) inputEl.value = st.input;
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
        var nx = ol + ev.clientX - sx, ny = ot + ev.clientY - sy;
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
  }

  function pendingRow(term) {
    var tr = document.createElement('tr');
    tr.innerHTML = '<td class="num"></td><td class="pend spin" colspan="8">查询中 · ' + esc(term) + '</td>';
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
    slot.tr.innerHTML = '<td class="num"></td><td class="pend spin" colspan="8">查询中 · ' + esc(slot.term) + '</td>';
    renumber();
  }

  function setSlotError(slot, errMsg) {
    slot.rows = []; slot.err = errMsg;
    var tr = slot.tr;
    tr.className = 'err';
    tr.innerHTML = '<td class="num"></td><td>—</td><td>' + esc(slot.term) + '</td>' +
      '<td colspan="5">' + esc(errMsg || '查询失败') + '</td><td></td>';
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
      slot.tr.innerHTML = '<td class="num"></td><td>—</td><td>' + esc(slot.term) + '</td>' +
        '<td colspan="5" class="pend">无结果</td><td></td>';
    } else {
      var first = slot.tr;
      keys.forEach(function (k, i) {
        var row = map[k];
        var tr = i === 0 ? first : document.createElement('tr');
        tr.className = '';
        tr.innerHTML =
          '<td class="num"></td>' +
          '<td>' + esc(row.g) + '</td>' +
          '<td>' + esc(row.a) + '</td>' +
          '<td class="ver" title="点击复制 ' + esc(row.g + ':' + row.a + ':' + row.ver) + '">' + esc(row.ver) + '</td>' +
          '<td class="tm">' + (row.ts ? fmtTs(row.ts) : '—') + '</td>' +
          '<td class="up" title="点击复制 ' + esc(row.a + '\t' + bumpVersion(row.ver)) + '">' + esc(bumpVersion(row.ver)) + '</td>' +
          '<td>' + esc(row.repo) + '</td>' +
          '<td class="num">' + row.n + '</td>' +
          '<td></td>';
        var act = tr.lastChild;
        act.appendChild(openBtn(row));
        var verTd = tr.querySelector('.ver'), upTd = tr.querySelector('.up');
        verTd.onclick = function () { copyText(row.g + ':' + row.a + ':' + row.ver); flash(verTd); };
        upTd.onclick = function () { copyText(row.a + '\t' + bumpVersion(row.ver)); flash(upTd); };
        if (i !== 0) first.parentNode.insertBefore(tr, first.nextSibling);
      });
    }
    renumber();
  }

  function flash(td) {
    var old = td.style.color;
    td.style.color = '#fff';
    setTimeout(function () { td.style.color = old; }, 300);
  }

  function querySlot(slot) {
    setSlotPending(slot);
    return searchOne(slot.term).then(function (hits) {
      setSlotResult(slot, hits);
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
