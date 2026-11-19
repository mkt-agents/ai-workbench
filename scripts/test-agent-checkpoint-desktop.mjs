import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile, access } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

const id = randomUUID();
const base = resolve('.build-tmp', `task-desktop-${id}`);
const workspace = join(base, 'repo');
const exe = resolve(process.argv[2] ?? 'src-tauri/target/debug/ai_workbench.exe');
const appIdentifier = 'com.ai-workbench.checkpoint-test';
const ports = { cdp: 14774, model: 14773 };
await mkdir(workspace, { recursive: true });
await mkdir(join(base, 'roaming'), { recursive: true });
await writeFile(join(workspace, 'sample.txt'), 'before\n');
const git = (...args) => execFileSync('git', args, { cwd: workspace, windowsHide: true });
git('init', '-b', 'checkpoint-test');
git('add', '.');
git('-c', 'user.name=Smoke Test', '-c', 'user.email=smoke@example.invalid', 'commit', '-m', 'fixture');
const sessionId = `smoke-${id}`;
const taskId = `task-${id}`;
const pending = new Set();
let phase = 'start';
let phaseCalls = 0;
let modelFailure;
let contextRequests = [];
let summaryMode = "ok";
let overflowReplies = 0;
const tool = (name, args) => ({ id: `call_${phase}_${phaseCalls}_${name}`, type: 'function', function: { name, arguments: JSON.stringify(args) } });
const server = createServer(async (req, res) => {
  try {
    const text = (await Array.fromAsync(req)).map(part => part.toString()).join('');
    const body = JSON.parse(text);
    assert.match(req.url, /chat\/completions/);
    const tools = (body.tools ?? []).map(t => t.function.name);
    const call = phaseCalls++;
    if (phase === "context") return respondToContext(body, res);
    if (phase === 'failure') {
      res.writeHead(401, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: { message: 'Intentional model failure' } }));
      return;
    }
    let calls = [];
    if (phase === 'start') {
      if (call > 0) { pending.add(res); return; }
      assert(tools.includes('write_file'));
      calls = [tool('write_file', { path: 'sample.txt', content: 'edited before restart\n' })];
    } else if (phase === 'read-only') {
      assert(!tools.includes('write_file'));
      if (call === 0) calls = [tool('write_file', { path: 'read-only.txt', content: 'must not exist' })];
    } else if (phase === 'resume') {
      if (call === 0) {
        assert(!tools.includes('write_file'));
        assert(!tools.includes('run_command'));
        assert(!body.messages.some(m => m.role === 'tool' || m.tool_calls));
        assert(body.messages.some(m => String(m.content).includes('Checkpoint smoke goal')));
        calls = [tool('write_file', { path: 'guarded.txt', content: 'must not exist' }), tool('read_file', { path: 'sample.txt' })];
      } else if (call === 1) {
        assert(body.messages.some(m => m.role === 'tool' && /external edit/.test(m.content)));
        calls = [tool('update_plan', { steps: [{ title: 'Verify the existing change', status: 'in_progress' }] })];
      } else if (call === 2) {
        calls = [tool('exit_plan_mode', { summary: 'Verify the file without changing it again' })];
      } else if (call === 3) {
        assert(tools.includes('run_command'));
        calls = [tool('run_command', { command: 'exit /b 7', verification: true })];
      } else if (call === 4) {
        calls = [tool('run_command', { command: 'type sample.txt', verification: true })];
      } else if (call === 5) {
        calls = [tool('run_command', { command: 'git diff -- sample.txt' })];
      }
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ choices: [{ index: 0, message: { role: 'assistant', content: calls.length ? '' : 'Verification finished.', ...(calls.length && { tool_calls: calls }) }, finish_reason: calls.length ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 300, completion_tokens: 20 } }));
  } catch (err) {
    modelFailure = err;
    res.writeHead(400).end(String(err));
  }
});
function respondToContext(body, res) {
  const prompt = String(body.messages.at(-1)?.content ?? '');
  const isSummary = prompt.startsWith('Update the summary from this conversation data:');
  contextRequests.push({ body, prompt, isSummary });
  const reply = (status, data) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(data)); };
  if (isSummary) {
    assert.equal(body.tools?.length ?? 0, 0);
    if (summaryMode === 'cancel') { pending.add(res); return; }
    if (summaryMode === 'fail') { reply(401, { error: { message: 'Intentional summary failure' } }); return; }
  } else if (prompt === 'overflow' && overflowReplies > 0) {
    overflowReplies--;
    reply(400, { error: { code: 'context_length_exceeded', message: 'Maximum context length exceeded' } });
    return;
  } else if (body.model === 'context-primary-fails') {
    reply(401, { error: { message: 'Intentional primary failure' } });
    return;
  }
  const content = isSummary ? 'Keep strict-constraint-42 and check sample.txt before editing.' : `Mock answer to ${prompt}`;
  reply(200, { choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }] });
}
await new Promise((ok, reject) => { server.once('error', reject); server.listen(ports.model, '127.0.0.1', ok); });

async function until(check, label, timeout = 15000) {
  let last;
  for (const deadline = Date.now() + timeout; Date.now() < deadline;) {
    if (modelFailure) throw modelFailure;
    try { const result = await check(); if (result) return result; } catch (e) { last = e; }
    await delay(300);
  }
  throw new Error(`${label} timed out${last ? ` (${last})` : ''}`);
}

async function tab(urlSuffix) {
  const target = await until(async () => {
    const tabs = await (await fetch(`http://127.0.0.1:${ports.cdp}/json/list`)).json();
    return tabs.find(t => t.url.startsWith('http://localhost:14772') && t.url.endsWith(urlSuffix));
  }, `WebView ${urlSuffix}`);
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((ok, no) => { ws.addEventListener('open', ok, { once: true }); ws.addEventListener('error', no, { once: true }); });
  let next = 0;
  const inflight = new Map();
  ws.addEventListener('message', ({ data }) => { const reply = JSON.parse(data); inflight.get(reply.id)?.(reply); });
  async function command(method, params) {
    const id = ++next;
    let timeout;
    const result = await new Promise((resolve, reject) => {
      timeout = setTimeout(() => reject(new Error(`${method} timed out`)), 20000);
      inflight.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    }).finally(() => { clearTimeout(timeout); inflight.delete(id); });
    if (result.error || result.result?.exceptionDetails) throw new Error(JSON.stringify(result.error || result.result.exceptionDetails));
    return result.result;
  }
  const evaluate = async (expression) => {
    // Keep a strong reference while WebView2 awaits the result over CDP.
    const retained = `(() => {
      const result = (0, eval)(${JSON.stringify(expression)});
      if (!result || typeof result.then !== 'function') return result;
      const pending = window.fixtureEvaluationPromises ??= new Set();
      pending.add(result);
      const release = () => setTimeout(() => pending.delete(result), 1000);
      void result.then(release, release);
      return result;
    })()`;
    return (await command('Runtime.evaluate', { expression: retained, awaitPromise: true, returnByValue: true })).result.value;
  };
  return { command, evaluate, close: () => ws.close() };
}
// Some WebView2 versions send Origin: null on the dev HTTP IPC transport.
// Exercise Tauri's native postMessage transport in this isolated fixture.
async function installIpcFallback(page) {
  const fallback = `(() => {
    const originalFetch = window.fetch.bind(window);
    window.fetch = (...args) => {
      const url = String(args[0]);
      const probe = window.__TAURI_INTERNALS__?.convertFileSrc?.('checkpoint-fixture-probe', 'ipc');
      const prefix = probe?.slice(0, probe.indexOf('checkpoint-fixture-probe'));
      if (prefix && url.startsWith(prefix)) return Promise.reject(new TypeError('Native IPC smoke fixture'));
      return originalFetch(...args);
    };
    window.checkpointFixtureIpcReady = true;
  })()`;
  await until(() => page.evaluate('location.href.startsWith("http://localhost:14772") && Boolean(window.__TAURI_INTERNALS__?.invoke)'), 'native IPC bootstrap');
  await page.evaluate(fallback);
}

async function click(page, selector) {
  await until(() => page.evaluate(`(() => {
    const button = document.querySelector(${JSON.stringify(selector)});
    if (!button || button.disabled) return false;
    button.click();
    return true;
  })()`), `click ${selector}`);
}

const taskDisclosure = '.qa-task-disclosure';
const badge = (hub, value) => hub.evaluate(`Boolean(document.querySelector('.qa-task-header [data-verification=${value}]'))`);

async function checkToolbar(hub, { restored = false } = {}) {
  const originalLocale = await hub.evaluate(`import('/src/core/store.ts').then(({ useGlobalStore }) => useGlobalStore.getState().settings.language)`);
  const changeLocale = locale => hub.evaluate(`import('/src/core/store.ts').then(({ useGlobalStore }) => useGlobalStore.getState().setSettings({ language: ${JSON.stringify(locale)} })).then(() => true)`);
  try {
    for (const [locale, details, labels, verificationLabel] of [
      ['zh-CN', '任务详情', ['任务目标', '工作区', '分支', '验证状态'], '验证通过'],
      ['en-US', 'Task details', ['Goal', 'Workspace', 'Branch', 'Verification'], 'Checks passed'],
    ]) {
      await changeLocale(locale);
      await until(() => hub.evaluate(`document.querySelector('${taskDisclosure}')?.textContent.trim() === ${JSON.stringify(details)}`), `toolbar locale ${locale}`);
      for (const width of [720, 640]) {
        await hub.command('Emulation.setDeviceMetricsOverride', { width, height: 640, deviceScaleFactor: 1, mobile: false });
        const layout = await hub.evaluate(`(() => {
          const card = document.querySelector('.qa-task-card');
          const row = card.querySelector('.qa-task-header');
          const disclosure = row.querySelector('.qa-task-disclosure');
          const resume = row.querySelector('.qa-task-resume');
          const meta = row.querySelector('.qa-thread-meta');
          const state = row.querySelector('.qa-task-state');
          const bounds = row.getBoundingClientRect();
          const controlsFit = [...row.querySelectorAll('button')].every(button => {
            const position = button.getBoundingClientRect();
            return position.width > 0 && position.left >= bounds.left && position.right <= bounds.right;
          });
          return {
            viewport: innerWidth, details: disclosure.textContent.trim(), controlsFit,
            collapsed: card.querySelector('.qa-task-details').hidden,
            resume: Boolean(resume), review: Boolean(row.querySelector('.qa-task-review')),
            hasMeta: Boolean(meta), sameRow: !meta || Math.abs(meta.getBoundingClientRect().top - state.getBoundingClientRect().top) < 5,
            oneRow: Math.abs(disclosure.getBoundingClientRect().top - state.getBoundingClientRect().top) < 7,
          };
        })()`);
        assert.equal(layout.viewport, width);
        assert.equal(layout.details, details);
        assert(layout.controlsFit && layout.collapsed, JSON.stringify(layout));
        assert.equal(layout.hasMeta, !restored);
        assert.equal(layout.resume, restored);
        assert.equal(layout.review, restored);
        assert(layout.sameRow, JSON.stringify(layout));
        // A stopped task can need a second line on narrow windows to keep the full reason and controls visible.
        if (!restored || width > 640) assert(layout.oneRow, JSON.stringify(layout));
        await click(hub, taskDisclosure);
        if (!restored) {
          await click(hub, '.qa-context-toggle');
          const panels = await hub.evaluate(`(() => {
            const context = document.querySelector('.qa-context-panel');
            const task = document.querySelector('.qa-task-details');
            return {
              bothOpen: !context.hidden && !task.hidden,
              stack: context.getBoundingClientRect().bottom <= task.getBoundingClientRect().top,
              fits: context.scrollWidth <= context.clientWidth,
            };
          })()`);
          assert(panels.bothOpen && panels.stack && panels.fits, JSON.stringify(panels));
          await click(hub, '.qa-context-toggle');
        }
        const facts = await hub.evaluate(`(() => {
          const panel = document.querySelector('.qa-task-details');
          const values = panel.querySelectorAll('.qa-task-facts dd');
          return {
            labels: [...panel.querySelectorAll('.qa-task-facts dt')].map(node => node.textContent.trim()),
            goal: values[0].textContent,
            workspace: values[1].textContent,
            branch: values[2].textContent,
            verification: panel.querySelector('[data-verification]').textContent.trim(),
            evidence: panel.querySelector('.qa-task-evidence code')?.textContent ?? null,
            exitCode: panel.querySelector('.qa-task-evidence > span:last-child')?.textContent ?? null,
            fits: !panel.hidden && panel.scrollWidth <= panel.clientWidth,
          };
        })()`);
        assert.deepEqual(facts.labels, labels);
        assert.equal(facts.goal, 'Checkpoint smoke goal');
        assert.equal(facts.workspace, workspace);
        assert.equal(facts.branch, 'checkpoint-test');
        assert(facts.fits, JSON.stringify(facts));
        if (!restored) {
          assert.equal(facts.verification, verificationLabel);
          assert.equal(facts.evidence, 'type sample.txt');
          assert.equal(facts.exitCode, locale === 'zh-CN' ? '退出码 0' : 'exit code 0');
        }
        await click(hub, taskDisclosure);
        if (process.env.CHECKPOINT_SCREENSHOTS === '1') {
          const screenshot = await hub.command('Page.captureScreenshot', { format: 'png' });
          const state = restored ? 'restored' : 'verified';
          await writeFile(join(base, `${state}-${locale}-${width}.png`), Buffer.from(screenshot.data, 'base64'));
        }
      }
    }
  } finally {
    await hub.command('Emulation.clearDeviceMetricsOverride');
    await changeLocale(originalLocale);
  }
}

let app;
async function launch() {
  app = spawn(exe, [], {
    windowsHide: true,
    env: { ...process.env, APPDATA: join(base, 'roaming'), WEBVIEW2_USER_DATA_FOLDER: join(base, 'webview2'), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${ports.cdp}` },
    stdio: 'ignore',
  });
  console.log('Started isolated fixture process', app.pid);
  const main = await tab('/');
  await installIpcFallback(main);
  await until(() => main.evaluate('Boolean(window.__TAURI_INTERNALS__?.invoke)'), 'main IPC');
  assert.equal(await main.evaluate(`window.__TAURI_INTERNALS__.invoke('plugin:app|identifier')`), appIdentifier, 'Build the dedicated fixture executable before running this test');
  await main.evaluate(`window.__TAURI_INTERNALS__.invoke('show_ai_hub')`);
  main.close();
  const hub = await tab('#/ai-hub');
  await installIpcFallback(hub);
  await until(() => hub.evaluate('Boolean(window.__TAURI_INTERNALS__?.invoke)'), 'Tianji IPC');
  return hub;
}
const invoke = (hub, command, args = {}) => hub.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`);
const getTask = hub => invoke(hub, 'agent_task_load', { sessionId });
const model = { id: 'checkpoint-mock', name: 'Checkpoint mock', provider: 'openai', apiKey: '', baseUrl: `http://127.0.0.1:${ports.model}/v1`, model: 'checkpoint-mock', temperature: 0, maxTokens: 4096, isDefault: true };
function startTask(hub, idOverride = taskId, requestId = `start-${id}`) {
  return hub.evaluate(`void window.__TAURI_INTERNALS__.invoke('agent_run_task', ${JSON.stringify({
    req: { config: model, user: 'Checkpoint smoke goal', system: 'Run only the mock model functions', requestId, repoPath: workspace, permission: 'workspace', tools: true, mcpEnabled: false },
    task: { sessionId, taskId: idOverride, goal: 'Checkpoint smoke goal' },
  })}).then(() => { window.fixtureOutcome = 'ok'; }, error => { window.fixtureOutcome = String(error); }); null`);
}
async function checkLongSession(hub) {
  phase = 'context';
  const now = new Date().toISOString();
  const create = async (name) => {
    const id = `smoke-${name}-${randomUUID()}`;
    await invoke(hub, 'agent_session_save', { id, title: name, updatedAt: now, create: true });
    return id;
  };
  const page = (id, before) => invoke(hub, 'agent_session_page', { id, before });
  const write = (id, revision, from, turns) => invoke(hub, 'agent_session_edit', { input: { id, revision, from, turns, updatedAt: now } });
  const round = (user, content, blocks) => [{ role: 'user', content: user }, { role: 'assistant', content, blocks }];
  const smallModel = { ...model, contextWindowTokens: 9000, maxTokens: 512 };
  const context = (sessionId, snapshot) => ({ sessionId, before: snapshot.total, revision: snapshot.revision });
  const run = (sessionId, snapshot, user, config = smallModel, fallbackConfig) => invoke(hub, 'agent_run_session', {
    context: context(sessionId, snapshot),
    req: { config, fallbackConfig, user, system: 'Mock project instructions', requestId: randomUUID(), repoPath: '', permission: 'read', tools: false, mcpEnabled: false, maxRetries: 0 },
  });
  const compactArgs = (id, snapshot, focus = 'sample.txt') => ({ context: context(id, snapshot), config: model, focus, contextTokens: 0, requestId: randomUUID() });
  const long = await create('120 short exchanges');
  let revision = 0;
  for (let i = 0; i < 120; i++) {
    const user = `question-original-${i}`;
    await run(long, { total: i * 2, revision }, user, model);
    const last = contextRequests.at(-1);
    assert.equal(last.prompt, user);
    revision = await write(long, revision, i * 2, round(user, `Mock answer to ${last.prompt}`));
  }
  const tail = await page(long);
  const middle = await page(long, tail.start);
  const head = await page(long, middle.start);
  assert.deepEqual([head.turns.length, middle.turns.length, tail.turns.length, tail.total], [40, 100, 100, 240]);
  assert.equal(head.turns[0].content, 'question-original-0');
  assert.equal(tail.turns.at(-1).content, 'Mock answer to question-original-119');
  assert((await invoke(hub, 'agent_session_search', { query: 'question-original-0' })).includes(long));
  await hub.evaluate("import('/src/core/store.ts').then(({ useGlobalStore }) => useGlobalStore.getState().loadAiHubSessions())");
  await click(hub, '.qa-history-btn');
  await until(() => hub.evaluate(`[...document.querySelectorAll('.qa-session-main')].find(button => button.textContent.includes('120 short exchanges'))?.click(), document.querySelector('.qa-session-item.active')?.textContent.includes('120 short exchanges') || !document.querySelector('.qa-history-panel')`), 'open long conversation');
  await until(() => hub.evaluate("document.querySelector('.qa-thread')?.textContent.includes('question-original-119')"), 'latest 50 exchanges rendered');
  assert.equal(await hub.evaluate("document.querySelector('.qa-thread').textContent.includes('question-original-0')"), false);
  // Only replace the save dialog at the store boundary. History reads and
  // Markdown generation still go through the production UI and SQLite IPC.
  await hub.evaluate(`import('/src/core/store.ts').then(({ useGlobalStore }) => useGlobalStore.setState({
    invokeSaveTextFile: async (content) => {
      window.capturedTranscript = content;
      return 'fixture-export.md';
    },
  }))`);
  await click(hub, 'button.qa-mini-btn:has(.lucide-download)');
  const markdown = await until(() => hub.evaluate('window.capturedTranscript'), 'export all original messages');
  assert.equal((markdown.match(/^## 🧑 You$/gm) ?? []).length, 120);
  assert.equal((markdown.match(/^## 🤖 Assistant$/gm) ?? []).length, 120);
  assert(markdown.includes('question-original-0') && markdown.includes('Mock answer to question-original-119'));
  await click(hub, '.qa-load-older');
  await until(() => hub.evaluate("document.querySelector('.qa-thread')?.textContent.includes('question-original-20')"), 'middle page rendered');
  await click(hub, '.qa-load-older');
  await until(() => hub.evaluate("document.querySelector('.qa-thread')?.textContent.includes('question-original-0')"), 'oldest page rendered');
  await click(hub, '.qa-track-btn:nth-child(2)');
  await until(() => hub.evaluate("document.querySelector('.qa-track-btn:nth-child(2)').classList.contains('is-active')"), 'Work uses the same conversation');
  const manualStart = contextRequests.length;
  await hub.evaluate(`(() => {
    const field = document.querySelector('.ai-hub-input');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(field, '/compact focus: sample.txt only');
    field.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await click(hub, '.qa-send-btn');
  await until(async () => (await page(long)).context.covered === 236, 'manual /compact completed');
  assert(contextRequests.slice(manualStart).some(request => request.isSummary && request.prompt.includes('focus: sample.txt only')));
  assert.equal((await page(long)).total, 240);
  console.log('120 replies, two history pages, Markdown export and /compact on Work passed');
  await click(hub, '.qa-track-btn:nth-child(1)');

  const big = await create('Big tool results');
  await write(big, 0, 0, Array.from({ length: 7 }, (_, i) => round(`strict-constraint-42 question-${i}`, i < 5 ? `original answer ${i} ${'历史'.repeat(1200)}` : `recent answer ${i}`, [
    { kind: 'tool_use', id: `pair-${i}`, name: 'read_file', input: { path: 'sample.txt' } },
    { kind: 'tool_result', toolUseId: `pair-${i}`, output: i < 5 ? '长期输出'.repeat(3600) : `recent result ${i}` },
    { kind: 'text', text: i < 5 ? `original answer ${i} ${'历史'.repeat(1200)}` : `recent answer ${i}` },
  ])).flat());
  const bigPage = await page(big);
  summaryMode = 'fail';
  let from = contextRequests.length;
  await run(big, bigPage, 'after-failure');
  assert(contextRequests.slice(from).some(request => request.isSummary));
  assert.equal((await page(big)).context.covered, 0);
  assert.equal(contextRequests.at(-1).prompt, 'after-failure');
  console.log('Large tool outputs and summary failure fallback passed');
  summaryMode = 'cancel';
  from = contextRequests.length;
  const cancel = compactArgs(big, bigPage);
  const cancelling = invoke(hub, 'agent_session_compact', cancel).then(() => '', error => String(error));
  await until(() => contextRequests.slice(from).some(request => request.isSummary), 'summary awaits cancellation');
  await invoke(hub, 'cancel_request', { id: cancel.requestId });
  assert.match(await cancelling, /AI_REQUEST_CANCELLED/);
  assert.equal((await page(big)).context.covered, 0);
  console.log('In-flight summary cancellation passed');
  summaryMode = 'ok';
  from = contextRequests.length;
  await run(big, bigPage, 'after-fallback', { ...model, model: 'context-primary-fails' }, smallModel);
  const fallback = contextRequests.slice(from);
  assert(fallback.some(request => request.body.model === 'context-primary-fails' && !request.isSummary));
  assert(fallback.some(request => request.body.model === smallModel.model && request.isSummary));
  const final = fallback.at(-1).body.messages;
  assert(final.some(message => String(message.content).includes('Keep strict-constraint-42')));
  assert(final.some(message => message.content === 'recent answer 6'));
  const liveCalls = final.flatMap(message => message.tool_calls ?? []);
  assert(liveCalls.length > 0);
  assert.deepEqual(liveCalls.map(call => call.id), final.filter(message => message.role === 'tool').map(message => message.tool_call_id));
  const bigCompacted = await page(big);
  assert(bigCompacted.context.covered > 0 && bigCompacted.total === 14);

  const retry = await create('Context overflow');
  await write(retry, 0, 0, Array.from({ length: 7 }, (_, i) => round(`question ${i}`, `answer ${i}: ${'details '.repeat(150)}`)).flat());
  from = contextRequests.length;
  overflowReplies = 2;
  await assert.rejects(run(retry, await page(retry), 'overflow'), /CONTEXT_REQUIRED_TOO_LARGE/);
  const attempted = contextRequests.slice(from).filter(request => request.prompt === 'overflow');
  assert.equal(attempted.length, 2);
  assert(JSON.stringify(attempted[1].body.messages).length < JSON.stringify(attempted[0].body.messages).length);
  assert.equal(overflowReplies, 0);

  hub.close();
  const exiting = new Promise(resolve => app.once('exit', resolve));
  app.kill('SIGKILL');
  await exiting;
  hub = await launch();
  const reopened = await page(long);
  assert.equal(reopened.context.covered, 236);
  assert(reopened.context.estimatedTokens > 0 && reopened.context.inputBudget > 0);
  assert.equal((await page(big)).context.covered, bigCompacted.context.covered);
  await write(big, bigCompacted.revision, 0, []);
  assert.equal((await page(big)).context.covered, 0);
  console.log('Desktop fake model: 120 replies, original pagination and search, focused compaction, cancellation, local fallback, window change, closed tools, one overflow retry and restart passed');
  return hub;
}
try {
  let hub = await launch();
  const now = new Date().toISOString();
  const sessions = await invoke(hub, 'db_load', { table: 'ai_hub_sessions' });
  assert(!sessions.length || sessions.every(row => row.id.startsWith('smoke-')), 'Isolated fixture must never touch normal user sessions');
  await invoke(hub, 'db_save', { table: 'ai_hub_sessions', rows: [{ id: sessionId, title: 'Checkpoint smoke fixture', task: 'none', turns: '[]', created_at: now, updated_at: now }] });
  await invoke(hub, 'db_save', { table: 'ai_models', rows: [{ id: model.id, name: model.name, provider: model.provider, api_key: '', base_url: model.baseUrl, model: model.model, temperature: 0, max_tokens: 4096, is_default: true, created_at: now, updated_at: now }] });
  await hub.evaluate(`localStorage.setItem('qa-permission', 'workspace')`);
  await startTask(hub);
  await until(async () => (await getTask(hub))?.actions.some(a => a.name === 'write_file' && a.status === 'done'), 'journaled desktop write');
  assert.equal((await readFile(join(workspace, 'sample.txt'), 'utf8')).trim(), 'edited before restart');
  assert.equal((await getTask(hub)).status, 'running');
  hub.close();
  await new Promise(resolve => { app.once('exit', resolve); app.kill('SIGKILL'); });
  for (const res of pending) res.destroy();
  console.log('Forced termination after persisted write');
  await writeFile(join(workspace, 'sample.txt'), 'external edit after crash\n');
  phase = 'resume';
  phaseCalls = 0;
  hub = await launch();
  const interrupted = await getTask(hub);
  assert.equal(interrupted?.status, 'interrupted');
  assert.equal(interrupted.reason, 'app_interrupted');
  assert.equal(interrupted.actions[0].status, 'done');
  await hub.evaluate(`import('/src/core/store.ts').then(({ useGlobalStore }) => Promise.all([useGlobalStore.getState().loadAiHubSessions(), useGlobalStore.getState().loadAIModels()])).then(() => true)`);
  await click(hub, '.qa-history-btn');
  await click(hub, '.qa-session-main');
  await until(() => hub.evaluate(`Boolean(document.querySelector('.qa-task-card button[aria-expanded="false"]'))`), 'restored task has collapsed keyboard disclosure');
  await checkToolbar(hub, { restored: true });
  assert(await badge(hub, 'unverified'));
  await hub.command('Page.bringToFront');
  assert(await hub.evaluate(`(() => {
    const disclosure = document.querySelector('${taskDisclosure}');
    disclosure.focus();
    return document.activeElement === disclosure;
  })()`));
  for (const [key, code, keyCode, text, expanded] of [
    ['Enter', 'Enter', 13, '\r', true], [' ', 'Space', 32, ' ', false],
  ]) {
    const event = { key, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode };
    await hub.command('Input.dispatchKeyEvent', { ...event, type: 'keyDown', text, unmodifiedText: text });
    await hub.command('Input.dispatchKeyEvent', { ...event, type: 'keyUp' });
    await until(() => hub.evaluate(`document.activeElement.getAttribute('aria-expanded') === '${expanded}'`), `${code} toggles details without losing focus`);
    assert.equal(await hub.evaluate(`document.querySelector('.qa-task-details').hidden`), !expanded);
  }
  phase = 'context';
  const transcript = await invoke(hub, 'agent_session_page', { id: sessionId });
  const archived = Array.from({ length: 5 }, (_, i) => [
    { role: 'user', content: `Archived request ${i}` },
    { role: 'assistant', content: `Archived answer ${i}`, blocks: [
      { kind: 'tool_use', id: `old-${i}`, name: 'write_file', input: { path: 'guarded.txt' } },
      { kind: 'tool_result', toolUseId: `old-${i}`, output: 'Archived tool outcome' },
    ] },
  ]).flat();
  const archivedRevision = await invoke(hub, 'agent_session_edit', { input: { id: sessionId, revision: transcript.revision, from: transcript.total, turns: archived, updatedAt: now } });
  const checkpointCompact = await invoke(hub, 'agent_session_compact', {
    context: { sessionId, before: transcript.total + archived.length, revision: archivedRevision },
    config: model, requestId: randomUUID(), contextTokens: 0, focus: 'sample.txt',
  });
  assert(checkpointCompact.compacted && checkpointCompact.covered > 0);
  await click(hub, '.qa-history-btn');
  await click(hub, '.qa-session-main');
  await until(() => hub.evaluate("document.querySelector('.qa-thread')?.textContent.includes('Archived request 4')"), 'compacted checkpoint conversation reopened');
  phase = 'resume';
  phaseCalls = 0;
  await click(hub, '.qa-task-resume');
  await until(async () => (await getTask(hub))?.status === 'waiting_approval', 'review approval');
  await until(() => hub.evaluate(`Boolean(document.querySelector('.qa-task-state[data-task-state=waiting_approval]'))`), 'waiting-for-approval badge rendered');
  await assert.rejects(access(join(workspace, 'guarded.txt')));
  await startTask(hub, taskId, `duplicate-${id}`);
  await until(async () => /TASK_ALREADY_RUNNING/.test(await hub.evaluate('window.fixtureOutcome')), 'duplicate-start rejection');
  await click(hub, '.qa-agent-approve.is-question .qa-agent-ask-opt');
  await click(hub, '.qa-agent-approve.is-question .qa-agent-allow');
  await until(async () => (await getTask(hub))?.status === 'waiting_approval', 'fresh command approval');
  await click(hub, '.qa-agent-approve:not(.is-question) .qa-agent-allow');
  await until(() => badge(hub, 'failed'), 'failed verification rendered');
  assert.equal((await getTask(hub)).verification.exitCode, 7);
  await click(hub, '.qa-agent-approve:not(.is-question) .qa-agent-allow');
  const finished = await until(async () => {
    const view = await getTask(hub);
    if (view?.status === 'interrupted') throw new Error(JSON.stringify(view));
    return view?.status === 'ended' && view;
  }, 'verified desktop task');
  assert.equal(finished.verification.status, 'passed');
  assert.equal(finished.verification.exitCode, 0);
  await until(() => badge(hub, 'passed'), 'passed verification rendered');
  await checkToolbar(hub);
  assert.equal(finished.actions.filter(a => a.name === 'write_file').length, 1);
  assert.equal((await readFile(join(workspace, 'sample.txt'), 'utf8')).trim(), 'external edit after crash');
  await assert.rejects(access(join(workspace, 'guarded.txt')));
  await writeFile(join(workspace, 'sample.txt'), 'changed after verification\n');
  assert.equal((await getTask(hub)).verification.status, 'stale');
  await hub.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await until(() => badge(hub, 'stale'), 'stale verification rendered');
  const permissionTask = `${taskId}-permission`;
  phase = 'failure';
  phaseCalls = 0;
  await startTask(hub, permissionTask, `failure-${id}`);
  await until(async () => (await getTask(hub))?.reason === 'model_error', 'interrupted model failure');
  await click(hub, '.qa-perm-pill');
  await click(hub, '.qa-perm-item.is-read');
  phase = 'read-only';
  phaseCalls = 0;
  await click(hub, '.qa-task-resume');
  await until(async () => (await getTask(hub))?.status === 'ended', 'resume with reduced permissions');
  await assert.rejects(access(join(workspace, 'read-only.txt')));
  assert.equal((await getTask(hub)).actions.length, 0);
  await until(() => hub.evaluate(`Boolean(document.querySelector('.qa-task-state[data-task-state=ended]'))`), 'read-only completion rendered');
  assert.equal(await badge(hub, 'unverified'), false);
  await click(hub, taskDisclosure);
  const message = await hub.evaluate(`document.querySelector('.qa-task-details:not([hidden]) [data-verification=unverified]')?.textContent.trim()`);
  assert(['未运行验证命令', 'No verification command run'].includes(message), message);
  assert.equal(await hub.evaluate(`Boolean(document.querySelector('.qa-task-evidence'))`), false);
  console.log('Desktop toolbar, keyboard disclosure, crash/restart, compaction before resume, review, command approval and verification passed');
  hub = await checkLongSession(hub);
  hub.close();
} finally {
  if (app && app.exitCode === null) app.kill('SIGKILL');
  for (const res of pending) res.destroy();
  server.closeAllConnections();
  server.close();
}




