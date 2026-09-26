// Sandbox execution of the injected capture script (extracted from wb_capture.rs).
// The script has no error surface inside WebView2, so running it for real is the
// only honest check that the paths still work after edits.
import { readFileSync } from "node:fs";
import vm from "node:vm";

const rs = readFileSync(new URL("../src-tauri/src/wb_capture.rs", import.meta.url), "utf8");
const start = rs.indexOf('const CAPTURE_SCRIPT: &str = r#"') + 'const CAPTURE_SCRIPT: &str = r#"'.length;
const end = rs.indexOf('"#;', start);
const template = rs.slice(start, end);
const VERIFY = "https://www.codebuddy.cn/console/api/client/v1/api-keys?page=1&page_size=1";
const script = template.replace("__WB_VERIFY_URL__", JSON.stringify(VERIFY));

let failures = 0;
function check(name, cond, extra = "") {
  if (cond) console.log(`  ok  ${name}`);
  else { failures++; console.log(`FAIL  ${name} ${extra}`); }
}

function payloadFromCaptureHref(href) {
  const u = new URL(href);
  return { kind: u.searchParams.get("kind"), via: u.searchParams.get("via"), v: u.searchParams.get("v") };
}

function makeSandbox({ pathname = "/profile/plan", verifyStatus = 200, fetchBody = "" } = {}) {
  const sent = [];
  const fetchCalls = [];
  const timers = [];
  const sess = new Map([["profile-enterpriseId", "ent-42"]]);
  const store = (m) => ({
    getItem: (k) => (m.has(k) ? m.get(k) : null),
    setItem: (k, v) => m.set(k, String(v)),
    key: (i) => [...m.keys()][i] ?? null,
    get length() { return m.size; },
  });

  const loc = { _href: `https://www.codebuddy.cn${pathname}`, search: "", pathname };
  Object.defineProperty(loc, "href", {
    get() { return loc._href; },
    set(v) {
      if (v.includes("aiwb-cb-capture.save")) { sent.push(payloadFromCaptureHref(v)); return; }
      loc._href = v;
      try {
        const u = new URL(v, "https://www.codebuddy.cn");
        loc.pathname = u.pathname;
      } catch { /* keep */ }
    },
  });

  const ctx = {
    console,
    URL,
    URLSearchParams,
    setInterval: (fn) => { timers.push(fn); return timers.length; },
    clearInterval: () => {},
    setTimeout: (fn) => { fn && fn(); return 0; },
    sessionStorage: store(sess),
    localStorage: store(new Map()),
    document: {
      cookie: "",
      body: { appendChild() {} },
      getElementById: () => null,
      createElement: () => ({ style: { cssText: "" } }),
      addEventListener() {},
    },
    fetch: (url, init) => {
      fetchCalls.push({ url: String(url), init });
      const isProbe = String(url).startsWith(VERIFY);
      return Promise.resolve({
        status: isProbe ? verifyStatus : 200,
        url: String(url),
        clone() { return this; },
        text: () => Promise.resolve(isProbe ? '{"items":[]}' : fetchBody),
      });
    },
    XMLHttpRequest: class {
      open(m, u) { this.__wbM = m; this.__wbU = u; this.__wbH = []; this._ls = []; }
      setRequestHeader(n, v) { if (this.__wbH) this.__wbH.push([n, v]); }
      send() {}
      addEventListener(t, fn) { if (t === "loadend") this._ls.push(fn); }
      fire() { this.status = this._status ?? 200; this.responseURL = this.__wbU; this.responseText = this._body ?? ""; this.responseType = this._rt ?? ""; this._ls.forEach((f) => f()); }
    },
  };
  ctx.location = loc;
  ctx.window = {
    ...ctx,
    top: ctx,
    location: loc,
    window: null,
  };
  ctx.window.window = ctx.window;
  vm.createContext(ctx);
  vm.runInContext(script, ctx);
  return { ctx, sent, fetchCalls, timers, sess };
}

async function tickN(sb, n) {
  for (const fn of [...sb.timers]) {
    for (let i = 0; i < n; i++) await fn();
  }
  await new Promise((r) => setImmediate(r));
}

// 1. header capture still works
{
  const sb = makeSandbox();
  const xhr = new sb.ctx.XMLHttpRequest();
  xhr.open("POST", "https://www.codebuddy.cn/console/api/x");
  xhr.setRequestHeader("authorization", "Bearer eyJhbCd.eyJhIjoxfQ.SIG");
  xhr.send();
  xhr.fire();
  check("header capture submits a bearer token", sb.sent.length === 1 && sb.sent[0].kind === "token", JSON.stringify(sb.sent));
}

// 2. XHR response body with a created API key is captured
{
  const sb = makeSandbox();
  const xhr = new sb.ctx.XMLHttpRequest();
  xhr._body = '{"data":{"id":9,"name":"k1","key":"cbk-abcd1234abcd1234abcd1234"}}';
  xhr.open("POST", "https://www.codebuddy.cn/console/api/client/v1/api-keys");
  xhr.send();
  xhr.fire();
  check("created API key captured from response body",
    sb.sent.length === 1 && sb.sent[0].kind === "token" && sb.sent[0].v === "cbk-abcd1234abcd1234abcd1234",
    JSON.stringify(sb.sent));
}

// 2b. cross-domain responses (QQ browser-ticket iframes) must never latch —
//     2026-09-26 实测：browsertdidticket.m.qq.com 的响应里带 "token" 字段，
//     无白名单时它第一个被当成凭证入库。
{
  const sb = makeSandbox();
  const qq = new sb.ctx.XMLHttpRequest();
  qq._body = '{"token":"AAAticketAAABBBBCCCCDDDDeee="+","ticket":"rI+Soiabcd1234abcd1234=="}';
  qq.open("POST", "https://browsertdidticket.m.qq.com/jp/ticket");
  qq.send();
  qq.fire();
  const qqIdle = sb.sent.length === 0;

  const track = new sb.ctx.XMLHttpRequest();
  track._body = '{"secret":"beacon-secret-beacon-secret-1234"}';
  track.open("GET", "https://report.qq.com/collect");
  track.send();
  track.fire();

  const rel = new sb.ctx.XMLHttpRequest();
  rel._body = '{"key":"cbk-rel0123456789abcdefghijklmnop"}';
  rel.open("GET", "/console/api/client/v1/api-keys");
  rel.send();
  rel.fire();

  check("QQ/tencent-tracker bodies are ignored (domain allowlist)",
    qqIdle && sb.sent.length === 1 && sb.sent[0].v === "cbk-rel0123456789abcdefghijklmnop",
    JSON.stringify(sb.sent));

  const sb3 = makeSandbox();
  const hdr = new sb3.ctx.XMLHttpRequest();
  hdr.open("POST", "https://browsertdidticket.m.qq.com/jp");
  hdr.setRequestHeader("authorization", "Bearer eyJhbCd.eyJhIjoxfQ.SIG");
  hdr.send();
  hdr.fire();
  check("auth headers from untrusted domains are ignored", sb3.sent.length === 0, JSON.stringify(sb3.sent));
}

// 3. fetch response body with a bare JWT is captured; clean bodies are not
{
  const sb = makeSandbox({ fetchBody: '{"access_token":"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcDEF123-_456"}' });
  await sb.ctx.window.fetch("https://www.codebuddy.cn/api/session");
  await new Promise((r) => setImmediate(r));
  check("JWT in fetch response body captured",
    sb.sent.length === 1 && sb.sent[0].v.startsWith("eyJ"), JSON.stringify(sb.sent));

  const sb2 = makeSandbox({ fetchBody: '{"ok":true,"plan":"pro"}' });
  await sb2.ctx.window.fetch("https://www.codebuddy.cn/api/session");
  await new Promise((r) => setImmediate(r));
  check("clean response body produces no capture", sb2.sent.length === 0, JSON.stringify(sb2.sent));
}

// 4. post-login bootstrap: navigates to /keys once, then verifies the session in-page
{
  const sb = makeSandbox({ pathname: "/profile/plan", verifyStatus: 200 });
  await tickN(sb, 4);
  check("bootstrap navigates to /keys", sb.ctx.location.pathname === "/keys", sb.ctx.location._href);
  const probe = sb.fetchCalls.find((c) => c.url.startsWith(VERIFY));
  check("in-page verify fetches probe url", !!probe, JSON.stringify(sb.fetchCalls));
  check("verify carries cookies + enterprise header",
    !!probe && probe.init.credentials === "include" && probe.init.headers["X-Enterprise-Id"] === "ent-42",
    JSON.stringify(probe && probe.init));
  const st = sb.ctx.window.__wbCaptureStats;
  check("stats report cookieOk", !!st && st.cookieOk === true && st.cookieProbe === 200, JSON.stringify(st));
  await tickN(sb, 2);
  const probes = sb.fetchCalls.filter((c) => c.url.startsWith(VERIFY));
  check("verify runs once, no loop", probes.length === 1 && sb.ctx.location.pathname === "/keys");
}

// 5. verify failing leaves cookieOk false (the UI must not auto-trust cookies)
{
  const sb = makeSandbox({ pathname: "/keys", verifyStatus: 401 });
  await tickN(sb, 3);
  const st = sb.ctx.window.__wbCaptureStats;
  check("401 session shows up in stats", st.cookieProbe === 401 && st.cookieOk === false, JSON.stringify(st));
  check("no navigation when already on /keys", sb.ctx.location.pathname === "/keys");
}

// 6. login page: no bootstrap, no verify
{
  const sb = makeSandbox({ pathname: "/login/", verifyStatus: 200 });
  await tickN(sb, 3);
  check("stays on login page", sb.ctx.location.pathname === "/login/");
  check("no verify before login", !sb.fetchCalls.some((c) => c.url.startsWith(VERIFY)));
}

console.log(failures === 0 ? "\nall sandbox cases passed" : `\n${failures} case(s) FAILED`);
process.exit(failures === 0 ? 0 : 1);
