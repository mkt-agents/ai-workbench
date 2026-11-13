// 内置 SkyWalking MCP server（stdio，换行分隔 JSON-RPC 2.0）。
//
// 由宿主（AI Workbench）随 ACP `session/new|resume` 的 mcpServers 下发给 DSH，
// 让 DSH 档像天玑档一样用 `skywalking_query` 工具查日志 / 调用链 / metrics，
// 替代旧的 `WECOM_LOG_QUERY:` 标记行回灌协议。
//
// 逻辑移植自 src-tauri/src/wecom_skywalking.rs（与预设脚本 v7.1.2 同协议）：
// - GraphQL 查询串硬编码，模型只能传 variables；
// - 值可信度一步不让：返回行逐条形状校验，坏行整页报错；
// - 进程生命周期内 HTTP 请求封顶 24 次、单请求 20s、响应 4MiB；
// - 成功响应按 (endpoint,header,query,variables) 缓存复用（singleflight 语义）。
//
// 配置来自宿主写入的 JSON 文件（argv[2]）：{ addr, header, tzOffsetMinutes }。
// 仅用 Node 内置能力（fetch 需 Node ≥ 18；DSH 本身依赖 Node）。

import { createInterface } from 'node:readline';
import { readFileSync } from 'node:fs';

// ---------- 配置 ----------

const CONFIG_PATH = process.argv[2];
if (!CONFIG_PATH) {
  process.stderr.write('skywalking-mcp: missing config path argument\n');
  process.exit(2);
}
let CFG;
try {
  CFG = JSON.parse(readFileSync(CONFIG_PATH, 'utf8'));
} catch (e) {
  process.stderr.write(`skywalking-mcp: bad config: ${e?.message || e}\n`);
  process.exit(2);
}
const ADDR = String(CFG.addr || '').trim();
const HEADER = String(CFG.header || '');
const CFG_TZ = CFG.tzOffsetMinutes == null ? null : Number(CFG.tzOffsetMinutes);

// ---------- 常量（与 wecom_skywalking.rs 对齐） ----------

const QUERY_LOGS = 'query queryLogs($condition: LogQueryCondition) { queryLogs(condition: $condition) { logs { serviceName serviceInstanceName endpointName traceId timestamp content tags { key value } } } }';
const QUERY_TRACE = 'query queryTrace($traceId: ID!) { trace: queryTrace(traceId: $traceId) { spans { traceId segmentId spanId parentSpanId refs { traceId parentSegmentId parentSpanId type } serviceCode serviceInstanceName endpointName type startTime endTime isError layer tags { key value } logs { time data { key value } } } } }';
const QUERY_FIND_SERVICE = 'query findService($serviceName: String!) { service: findService(serviceName: $serviceName) { id name } }';
const QUERY_SORT_METRICS = 'query sortMetrics($condition: TopNCondition!, $duration: Duration!) { records: sortMetrics(condition: $condition, duration: $duration) { name id value } }';
const QUERY_INSTANCES = 'query listInstances($serviceId: ID!, $duration: Duration!) { instances: listInstances(serviceId: $serviceId, duration: $duration) { id name } }';
const QUERY_LIST_METRICS = 'query listMetrics($regex: String) { metrics: listMetrics(regex: $regex) { name } }';

const DEFAULT_WINDOW_MIN = 240;
const REQUEST_TIMEOUT_MS = 20_000;
const MAX_QUERY_REQUESTS = 24;
const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const MAX_LOGS = 40;
const MAX_SPANS = 120;
const MAX_PAGE_SIZE = 100;
const METRICS_TOPN = 10;
const MAX_TRACE_IDS = 8;

// ---------- 预算与缓存 ----------

let requestCount = 0;
let cacheHits = 0;
const cache = new Map();

// ---------- 通用工具 ----------

function strArg(args, key) {
  const v = args?.[key];
  return typeof v === 'string' ? v.trim() : '';
}

function integerArg(value) {
  if (typeof value === 'number' && Number.isFinite(value) && Number.isInteger(value)) return value;
  if (typeof value === 'string' && /^-?\d+$/.test(value.trim())) return Number(value.trim());
  return null;
}

function optionalString(obj, key) {
  const v = obj?.[key];
  if (v === undefined || v === null) return;
  if (typeof v !== 'string') throw new Error(`schema_error: ${key} must be a string`);
}

function validateTags(value) {
  if (value === undefined || value === null) return;
  if (!Array.isArray(value)) throw new Error('schema_error: tags must be an array');
  for (const t of value) {
    if (!t || typeof t !== 'object' || Array.isArray(t)
      || typeof t.key !== 'string' || typeof t.value !== 'string') {
      throw new Error('schema_error: tag must be {key: string, value: string}');
    }
  }
}

function parseHeaders(header) {
  const out = [];
  for (const line of header.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf(':');
    if (idx <= 0) continue;
    out.push([trimmed.slice(0, idx).trim(), trimmed.slice(idx + 1).trim()]);
  }
  return out;
}

function graphqlEndpoint(addr) {
  const trimmed = addr.trim();
  if (!/^https?:\/\/.+/i.test(trimmed)) {
    throw new Error('source_endpoint_error: expected an absolute HTTP(S) endpoint');
  }
  const path = trimmed.replace(/\/+$/, '');
  return path.endsWith('/graphql') ? path : `${path}/graphql`;
}

function sanitizedEndpoint(url) {
  try {
    const u = new URL(url);
    u.username = '';
    u.password = '';
    return u.toString();
  } catch {
    return '[invalid endpoint]';
  }
}

function localOffsetMinutes() {
  return -new Date().getTimezoneOffset();
}

function resolveOffsetMinutes(args) {
  if (args && args.timezone_offset_minutes != null) {
    const m = integerArg(args.timezone_offset_minutes);
    if (m === null || m < -1439 || m > 1439) {
      throw new Error('schema_error: timezone_offset_minutes must be an integer within -1439..1439');
    }
    return m;
  }
  if (CFG_TZ != null && Number.isFinite(CFG_TZ)) return CFG_TZ;
  return localOffsetMinutes();
}

// 时间格式化到固定 UTC 偏移：`YYYY-MM-DD HHmmss`（OAP Duration 语义）。
function fmtOffset(d, offsetMin) {
  const shifted = new Date(d.getTime() + offsetMin * 60_000);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${shifted.getUTCFullYear()}-${p(shifted.getUTCMonth() + 1)}-${p(shifted.getUTCDate())} `
    + `${p(shifted.getUTCHours())}${p(shifted.getUTCMinutes())}${p(shifted.getUTCSeconds())}`;
}

// epoch 毫秒 → 本地 `MM-DD HH:MM:SS`（与 Rust 侧 fmt_ts 一致，用本机时区）。
function fmtTs(ms) {
  if (!(ms > 0)) return '--';
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function b64(s) {
  return Buffer.from(s, 'utf8').toString('base64');
}

// SkyWalking serviceId 确定性编码：`base64(服务名) + "." + layer`（GENERAL layer=1）。
function serviceId(service) {
  return `${b64(service)}.1`;
}

// endpointId 编码：`serviceId_base64(endpoint名)`。
function endpointId(serviceIdStr, endpoint) {
  return `${serviceIdStr}_${b64(endpoint)}`;
}

function utf8Chars(s, n) {
  return Array.from(s).slice(0, n).join('');
}

// ---------- GraphQL ----------

function cacheKey(url, header, query, variables) {
  return JSON.stringify([url, header, query, variables]);
}

function cacheableResponse(query, body) {
  if (!body || typeof body !== 'object') return false;
  if (query === QUERY_TRACE) {
    return Array.isArray(body?.data?.trace?.spans);
  }
  if (query === QUERY_LOGS) {
    return Array.isArray(body?.data?.queryLogs?.logs);
  }
  if (query === QUERY_FIND_SERVICE) {
    return body?.data?.service == null || typeof body?.data?.service?.id === 'string';
  }
  if (query === QUERY_SORT_METRICS || query === QUERY_INSTANCES || query === QUERY_LIST_METRICS) {
    return true;
  }
  return false;
}

async function graphql(url, header, query, variables) {
  if (requestCount >= MAX_QUERY_REQUESTS) {
    throw new Error(`budget_exceeded: 本轮 SkyWalking 查询已达 ${MAX_QUERY_REQUESTS} 次，请基于现有证据作答。`);
  }
  const endpoint = graphqlEndpoint(url);
  const key = cacheKey(endpoint, header, query, variables);
  const cached = cache.get(key);
  if (cached) {
    cacheHits += 1;
    return cached;
  }
  requestCount += 1;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  let body;
  try {
    const headers = { 'content-type': 'application/json' };
    for (const [k, v] of parseHeaders(header)) headers[k] = v;
    const resp = await fetch(endpoint, {
      method: 'POST',
      headers,
      body: JSON.stringify({ query, variables }),
      signal: controller.signal,
    });
    if (!resp.ok) {
      throw new Error(`transport_error: SkyWalking HTTP ${resp.status}`);
    }
    const len = Number(resp.headers.get('content-length') || '0');
    if (len > MAX_RESPONSE_BYTES) {
      throw new Error(`schema_error: response exceeds 4 MiB; endpoint=${sanitizedEndpoint(endpoint)}`);
    }
    const text = await resp.text();
    if (Buffer.byteLength(text, 'utf8') > MAX_RESPONSE_BYTES) {
      throw new Error(`schema_error: response exceeds 4 MiB; endpoint=${sanitizedEndpoint(endpoint)}`);
    }
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new Error(`schema_error: response is not JSON; endpoint=${sanitizedEndpoint(endpoint)}`);
    }
    if (parsed?.errors) {
      throw new Error(`graphql_error: OAP 返回错误（字段或参数可能不被该版本支持）`);
    }
    body = parsed;
  } catch (e) {
    if (e?.name === 'AbortError') {
      throw new Error(`timeout: SkyWalking request exceeded 20s; endpoint=${sanitizedEndpoint(endpoint)}`);
    }
    if (e instanceof Error && /^(\w+_(error|exceeded)|graphql_error|timeout)/.test(e.message)) throw e;
    throw new Error(`transport_error: SkyWalking HTTP request failed; endpoint=${sanitizedEndpoint(endpoint)}`);
  } finally {
    clearTimeout(timer);
  }
  if (cacheableResponse(query, body)) cache.set(key, body);
  return body;
}

function pointer(obj, path) {
  let cur = obj;
  for (const seg of path.replace(/^\//, '').split('/')) {
    if (cur == null || typeof cur !== 'object') return undefined;
    cur = cur[seg];
  }
  return cur;
}

// ---------- 时间窗 ----------

function queryWindow(args, defaultMinutes) {
  const offsetMin = resolveOffsetMinutes(args);
  let start;
  let end;
  const hasStart = args && args.start != null && String(args.start).trim() !== '';
  const hasEnd = args && args.end != null && String(args.end).trim() !== '';
  if (!hasStart && !hasEnd) {
    let minutes = defaultMinutes;
    if (args && args.minutes != null) {
      const m = integerArg(args.minutes);
      if (m !== null && m > 0 && m <= 24 * 60) minutes = m;
    }
    end = new Date();
    start = new Date(end.getTime() - minutes * 60_000);
  } else if (hasStart && hasEnd) {
    const parse = (v) => {
      const t = Date.parse(String(v).trim());
      if (Number.isNaN(t)) {
        throw new Error('schema_error: start/end must include RFC3339 timezone');
      }
      return new Date(t);
    };
    start = parse(args.start);
    end = parse(args.end);
  } else {
    throw new Error('schema_error: start and end must be supplied together');
  }
  if (start.getTime() > end.getTime()) {
    throw new Error('schema_error: start must not be after end');
  }
  const duration = () => ({
    start: fmtOffset(start, offsetMin),
    end: fmtOffset(end, offsetMin),
    step: 'SECOND',
  });
  return { startMs: start.getTime(), endMs: end.getTime(), offsetMin, duration };
}

// ---------- 日志 ----------

function parseKeywords(args) {
  if (Array.isArray(args?.keywords)) {
    return args.keywords
      .filter((v) => typeof v === 'string')
      .map((s) => s.trim())
      .filter(Boolean);
  }
  return strArg(args, 'keyword')
    .split(/[，,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function parseLogRow(log) {
  if (!log || typeof log !== 'object' || Array.isArray(log)) {
    throw new Error('schema_error: log must be an object');
  }
  const ts = integerArg(log.timestamp);
  if (ts === null) throw new Error('schema_error: log.timestamp must be epoch milliseconds');
  for (const key of ['serviceName', 'content']) {
    if (typeof log[key] !== 'string') throw new Error(`schema_error: log.${key} must be a string`);
  }
  for (const key of ['endpointName', 'serviceInstanceName', 'traceId']) optionalString(log, key);
  validateTags(log.tags);
  let level = '';
  let thread = '';
  let logger = '';
  if (Array.isArray(log.tags)) {
    for (const t of log.tags) {
      const k = (t.key || '').toLowerCase();
      const v = t.value || '';
      if (k === 'level') level = v;
      else if (k === 'thread') thread = v;
      else if (k === 'logger') logger = v;
    }
  }
  return {
    ts,
    service: log.serviceName || '',
    instance: typeof log.serviceInstanceName === 'string' ? log.serviceInstanceName : '',
    endpoint: typeof log.endpointName === 'string' ? log.endpointName : '',
    level,
    thread,
    logger,
    traceId: typeof log.traceId === 'string' ? log.traceId : '',
    content: (log.content || '').trim(),
  };
}

function buildLogCondition(serviceIdStr, endpointIdStr, keywords, exclude, level, traceId, duration, page, pageSize) {
  const condition = {
    paging: { pageNum: page, pageSize },
    queryOrder: 'DES',
  };
  if (traceId) {
    // relatedTrace 与 queryDuration/serviceId 互斥。
    condition.relatedTrace = { traceId };
  } else {
    condition.queryDuration = duration;
    if (serviceIdStr) {
      condition.serviceId = serviceIdStr;
      if (endpointIdStr) condition.endpointId = endpointIdStr;
    }
    if (keywords.length) condition.keywordsOfContent = keywords;
    if (exclude) condition.excludingKeywordsOfContent = [exclude];
    if (level) condition.tags = [{ key: 'level', value: level }];
  }
  return condition;
}

async function resolveServiceId(addr, header, service) {
  const body = await graphql(addr, header, QUERY_FIND_SERVICE, { serviceName: service });
  const found = pointer(body, '/data/service');
  if (found === undefined) throw new Error('schema_error: data.service missing');
  if (found == null) return serviceId(service); // 服务不存在时用旧编码兜底
  if (typeof found.id !== 'string' || !found.id) {
    throw new Error('schema_error: data.service.id must be a nonempty string');
  }
  return found.id;
}

async function fetchLogsPage(addr, header, args, window) {
  const service = strArg(args, 'service');
  const endpoint = strArg(args, 'endpoint');
  const keywords = parseKeywords(args);
  const exclude = strArg(args, 'exclude_keyword');
  const level = strArg(args, 'level');
  const traceId = strArg(args, 'trace_id');
  let page = 1;
  if (args?.page != null) {
    const p = integerArg(args.page);
    if (p !== null && p >= 1) page = p;
  }
  let pageSize = MAX_LOGS;
  if (args?.page_size != null) {
    const s = integerArg(args.page_size);
    if (s !== null && s >= 1) pageSize = Math.min(s, MAX_PAGE_SIZE);
  }

  // serviceName → 真实 id（trace 查询不需要）。
  let sid = '';
  let eid = '';
  if (!traceId && service) {
    sid = await resolveServiceId(addr, header, service);
    eid = endpoint ? endpointId(sid, endpoint) : '';
  }

  const condition = buildLogCondition(
    sid, eid, keywords, exclude, level, traceId, window.duration(), page, pageSize,
  );
  const body = await graphql(addr, header, QUERY_LOGS, { condition });
  const logs = pointer(body, '/data/queryLogs/logs');
  if (!Array.isArray(logs)) throw new Error('schema_error: data.queryLogs.logs must be an array');
  // 先验证整页再筛选，坏行即使落在窗口之外也不能被静默吞掉。
  const rows = logs.map(parseLogRow);
  const serviceFilter = traceId ? service : '';
  return {
    fetchedCount: logs.length,
    rows: rows.filter((row) => row.ts >= window.startMs
      && row.ts <= window.endMs
      && (!serviceFilter || row.service === serviceFilter)),
  };
}

function isErrorRow(r) {
  const lvl = r.level.toUpperCase();
  if (['ERROR', 'FATAL', 'SEVERE', 'ERR'].includes(lvl)) return true;
  const c = r.content.toLowerCase();
  return c.includes('exception')
    || c.includes('error')
    || c.includes('traceback')
    || c.includes('caused by')
    || r.content.includes('异常')
    || r.content.includes('报错');
}

// 本页错误级条数 + 去重 traceId（错误行的 traceId 优先靠前）。
function summarizeLogs(rows) {
  let errCount = 0;
  const ids = [];
  for (const onlyErr of [true, false]) {
    for (const r of rows) {
      const isErr = isErrorRow(r);
      if (onlyErr) {
        if (isErr) errCount += 1;
        if (!isErr) continue;
      } else if (isErr) {
        continue;
      }
      const tid = r.traceId.trim();
      if (tid && !ids.includes(tid) && ids.length < MAX_TRACE_IDS) ids.push(tid);
    }
  }
  return [errCount, ids];
}

function noneIfEmpty(s) {
  return s || '无';
}

function formatLogs(rows, minutes, page, args, window) {
  const service = strArg(args, 'service');
  const endpoint = strArg(args, 'endpoint');
  const keywords = parseKeywords(args);
  const keywordStr = keywords.join('+');
  const exclude = strArg(args, 'exclude_keyword');
  const level = strArg(args, 'level');
  const traceId = strArg(args, 'trace_id');
  const hasExplicit = strArg(args, 'start') !== '';
  const windowLabel = hasExplicit
    ? `${strArg(args, 'start')} 至 ${strArg(args, 'end')}`
    : `最近 ${minutes} 分钟`;

  if (!rows.length) {
    return `SkyWalking 本页筛选后无日志（${windowLabel}，第 ${page} 页，service=${noneIfEmpty(service)} endpoint=${noneIfEmpty(endpoint)} keyword=${noneIfEmpty(keywordStr)} exclude=${noneIfEmpty(exclude)} level=${noneIfEmpty(level)} trace=${noneIfEmpty(traceId)}）。本页为空不代表后续页为空；可继续翻页或放宽条件。`;
  }

  const [errCount, traceIds] = summarizeLogs(rows);
  let out = `SkyWalking 日志（${windowLabel}，第 ${page} 页，筛选后 ${rows.length} 条）：\n`;
  if (errCount > 0 || traceIds.length) {
    out += `摘要：错误级 ${errCount} 条`;
    if (traceIds.length) out += `；可追查 traceId（优先错误）：${traceIds.join(', ')}`;
    out += '\n';
  }
  for (const r of rows) {
    const lvl = r.level ? `[${r.level.toUpperCase()}] ` : '';
    const svc = r.service || '-';
    const ep = r.endpoint || '-';
    let meta = '';
    if (r.instance) meta += ` · instance=${r.instance}`;
    if (r.logger) meta += ` · @${r.logger}`;
    if (r.thread) meta += ` · ${r.thread}`;
    if (r.traceId) meta += ` · trace=${r.traceId}`;
    const content = utf8Chars(r.content, 500);
    out += `- ${lvl}${fmtTs(r.ts)} ${svc} · ${ep}${meta}\n  ${content}\n`;
  }
  out += '\n排障提示：先看带 [ERROR] 的行及其 traceId，用该 trace_id 查调用链（kind 省略即返回调用链）定位根因；还要更多日志传 page 翻页；要端点/实例趋势传 kind=metrics。';
  return out;
}

async function queryLogs(addr, header, args) {
  let minutes = DEFAULT_WINDOW_MIN;
  if (args?.minutes != null) {
    const m = integerArg(args.minutes);
    if (m !== null && m > 0 && m <= 24 * 60) minutes = m;
  }
  let page = 1;
  if (args?.page != null) {
    const p = integerArg(args.page);
    if (p !== null && p >= 1) page = p;
  }
  const window = queryWindow(args, DEFAULT_WINDOW_MIN);
  const result = await fetchLogsPage(addr, header, args, window);
  return `${formatLogs(result.rows, minutes, page, args, window)}\nOAP 本页原始条数：${result.fetchedCount}；分页位置基于原始页，非筛选后条数。`;
}

// ---------- 调用链 ----------

function parseTraceSpans(body) {
  const spans = pointer(body, '/data/trace/spans');
  if (!Array.isArray(spans)) throw new Error('schema_error: data.trace.spans must be an array');
  for (const span of spans) {
    for (const key of ['segmentId', 'serviceCode', 'endpointName']) {
      if (typeof span?.[key] !== 'string') throw new Error(`schema_error: span.${key} must be a string`);
    }
    for (const key of ['spanId', 'parentSpanId', 'startTime', 'endTime']) {
      if (integerArg(span?.[key]) === null) throw new Error(`schema_error: span.${key} must be an integer`);
    }
    if (typeof span?.isError !== 'boolean') throw new Error('schema_error: span.isError must be a boolean');
    for (const key of ['traceId', 'serviceInstanceName', 'type', 'layer']) optionalString(span, key);
    validateTags(span.tags);
    if (span.refs != null) {
      if (!Array.isArray(span.refs)) throw new Error('schema_error: span.refs must be an array');
      for (const r of span.refs) {
        if (typeof r?.parentSegmentId !== 'string' || integerArg(r?.parentSpanId) === null) {
          throw new Error('schema_error: malformed span ref');
        }
        optionalString(r, 'traceId');
        optionalString(r, 'type');
      }
    }
    if (span.logs != null) {
      if (!Array.isArray(span.logs)) throw new Error('schema_error: span.logs must be an array');
      for (const entry of span.logs) {
        if (integerArg(entry?.time) === null || !Array.isArray(entry?.data)) {
          throw new Error('schema_error: malformed span log');
        }
        validateTags(entry.data);
      }
    }
  }
  return spans;
}

// 超过 MAX_SPANS 时：保留错误 span 优先，再补它们的祖先，剩余名额按原顺序填满。
function selectDisplaySpans(spans) {
  if (spans.length <= MAX_SPANS) return spans;
  const index = new Map();
  spans.forEach((span, i) => {
    const key = `${strArg(span, 'segmentId')}:${integerArg(span.spanId) ?? -1}`;
    if (!index.has(key)) index.set(key, []);
    index.get(key).push(i);
  });
  const selected = new Array(spans.length).fill(false);
  const queue = [];
  let count = 0;
  // 先容纳所有错误，避免一条长祖先链挤掉别的错误。
  spans.forEach((span, i) => {
    if (span.isError === true && count < MAX_SPANS) {
      selected[i] = true;
      count += 1;
      queue.push(i);
    }
  });
  while (queue.length) {
    const i = queue.shift();
    const span = spans[i];
    const parents = [];
    if (Array.isArray(span.refs)) {
      for (const r of span.refs) {
        parents.push(`${strArg(r, 'parentSegmentId')}:${integerArg(r.parentSpanId) ?? -1}`);
      }
    }
    const selfParent = integerArg(span.parentSpanId);
    if (selfParent !== null && selfParent >= 0) {
      parents.push(`${strArg(span, 'segmentId')}:${selfParent}`);
    }
    for (const key of parents) {
      for (const p of index.get(key) || []) {
        if (!selected[p] && count < MAX_SPANS) {
          selected[p] = true;
          count += 1;
          queue.push(p);
        }
      }
    }
  }
  for (let i = 0; i < selected.length && count < MAX_SPANS; i += 1) {
    if (!selected[i]) {
      selected[i] = true;
      count += 1;
    }
  }
  return spans.filter((_, i) => selected[i]);
}

function renderSpan(span, depth) {
  const indent = '  '.repeat(depth);
  const svc = span.serviceCode || '-';
  const ep = span.endpointName || '-';
  const isErr = span.isError === true;
  const start = integerArg(span.startTime) ?? 0;
  const end = integerArg(span.endTime) ?? 0;
  const cost = Math.max(end - start, 0);
  const flag = isErr ? '❌ERROR' : 'ok';
  let line = `${indent}- ${flag} ${svc} · ${ep} (${cost}ms)\n`;
  if (Array.isArray(span.refs) && span.refs.length > 1) {
    for (const r of span.refs) {
      line += `${indent}    ref=${strArg(r, 'parentSegmentId')} : ${integerArg(r.parentSpanId) ?? -1} [${strArg(r, 'type')}]\n`;
    }
  }
  if (isErr) {
    const interesting = new Set(['status_code', 'http.status_code', 'error', 'db.statement', 'rpc.status_code']);
    if (Array.isArray(span.tags)) {
      for (const kv of span.tags) {
        const k = kv?.key || '';
        const v = kv?.value || '';
        if (interesting.has(k) && v.trim()) {
          line += `${indent}    ${k}: ${utf8Chars(v, 200)}\n`;
        }
      }
    }
    if (Array.isArray(span.logs)) {
      for (const entry of span.logs.slice(0, 3)) {
        if (Array.isArray(entry?.data)) {
          for (const kv of entry.data.slice(0, 6)) {
            const k = kv?.key || '';
            const v = kv?.value || '';
            if (v.trim()) line += `${indent}    ${k}: ${utf8Chars(v, 400)}\n`;
          }
        }
      }
    }
  }
  return line;
}

// 用 segmentId+spanId 建索引，按 refs（跨段父子）/ parentSpanId（同段父子）重建调用树。
// 返回每行已带缩进与出错明细的文本行（顺序 = 前序遍历，根按 startTime 排）。
function buildSpanTree(spans) {
  const index = new Map();
  const ambiguous = new Set();
  spans.forEach((s, i) => {
    const key = `${strArg(s, 'segmentId')}:${integerArg(s.spanId) ?? -1}`;
    if (index.has(key)) ambiguous.add(key);
    else index.set(key, i);
  });
  for (const key of ambiguous) index.delete(key);
  const notes = new Array(spans.length).fill('');
  const parent = new Array(spans.length).fill(null);
  const children = spans.map(() => []);
  spans.forEach((s, i) => {
    const seg = strArg(s, 'segmentId');
    const parentSpan = integerArg(s.parentSpanId) ?? -1;
    let p = null;
    const refs = Array.isArray(s.refs) ? s.refs : [];
    if (refs.length === 1) {
      const r = refs[0];
      p = index.get(`${strArg(r, 'parentSegmentId')}:${integerArg(r.parentSpanId) ?? -1}`) ?? null;
      if (p === null) notes[i] = '父关系未解析（引用缺失或身份重复）';
    } else if (refs.length > 1) {
      notes[i] = '多个 refs：关联已列出，唯一父关系未确定';
    } else if (parentSpan >= 0) {
      p = index.get(`${seg}:${parentSpan}`) ?? null;
      if (p === null) notes[i] = '父关系未解析（父节点缺失或身份重复）';
    }
    if (p === i) {
      p = null;
      notes[i] = '父关系异常：自引用';
    }
    parent[i] = p;
    if (p !== null) children[p].push(i);
  });
  const startOf = (i) => integerArg(spans[i].startTime) ?? 0;
  const roots = spans.map((_, i) => i).filter((i) => parent[i] === null).sort((a, b) => startOf(a) - startOf(b));
  for (const kids of children) kids.sort((a, b) => startOf(a) - startOf(b));
  // 前序遍历（显式栈，防极端深度递归）。
  const lines = [];
  const stack = [...roots].reverse().map((i) => [i, 0]);
  const visited = new Array(spans.length).fill(false);
  while (stack.length) {
    const [i, depth] = stack.pop();
    if (visited[i]) continue;
    visited[i] = true;
    lines.push(renderSpan(spans[i], depth));
    if (notes[i]) lines.push(`${'  '.repeat(depth)}  [${notes[i]}]\n`);
    for (const c of [...children[i]].reverse()) {
      if (!visited[c]) stack.push([c, depth + 1]);
    }
  }
  // 兜底：环引用导致未访问到的 span 也补在后面，不吞数据。
  spans.forEach((s, i) => {
    if (!visited[i]) {
      lines.push(`${renderSpan(s, 0)}  [父关系未解析：环引用或连接到环]\n`);
    }
  });
  return lines;
}

function formatTraceSpans(spans) {
  const total = spans.length;
  const errCount = spans.filter((s) => s.isError === true).length;
  let out = `SkyWalking 调用链（共 ${total} 个 span，其中 ${errCount} 个报错，最多展示 ${MAX_SPANS}；按父子层级缩进）：\n`;
  const selected = selectDisplaySpans(spans);
  for (const line of buildSpanTree(selected)) out += line;
  if (selected.length < spans.length) {
    out += `\n展示截断：保留错误 span 优先、随后其祖先，省略 ${spans.length - selected.length} 个 span；缺失父节点可能位于省略部分。\n`;
  }
  if (errCount > 0) {
    out += '\n定位到报错 span 后，可用其 serviceCode + 时间窗再查日志（传 service / keyword），或传 kind=metrics 看端点/实例趋势。';
  }
  return out;
}

async function queryTrace(addr, header, traceId) {
  if (!traceId.trim()) throw new Error('schema_error: trace_id must not be empty');
  const body = await graphql(addr, header, QUERY_TRACE, { traceId: traceId.trim() });
  const spans = parseTraceSpans(body);
  if (!spans.length) {
    return `SkyWalking 未查到调用链 trace=${traceId}（可能已过期或 traceId 有误）。`;
  }
  return `SkyWalking trace=${traceId}\n${formatTraceSpans(spans)}`;
}

// ---------- 指标诊断 ----------

async function listMetricNames(addr, header) {
  const body = await graphql(addr, header, QUERY_LIST_METRICS, { regex: '' });
  const rows = pointer(body, '/data/metrics');
  if (!Array.isArray(rows)) throw new Error('schema_error: data.metrics must be an array');
  return rows.map((m) => {
    if (typeof m?.name !== 'string') throw new Error('schema_error: metric.name must be a string');
    return m.name;
  });
}

async function topEndpoints(addr, header, metric, service, duration) {
  const body = await graphql(addr, header, QUERY_SORT_METRICS, {
    condition: {
      name: metric,
      parentService: service,
      normal: true,
      scope: 'Endpoint',
      topN: METRICS_TOPN,
      order: 'DES',
    },
    duration,
  });
  const rows = pointer(body, '/data/records');
  if (!Array.isArray(rows)) throw new Error('schema_error: data.records must be an array');
  return rows.map((r) => {
    if (typeof r?.name !== 'string') throw new Error('schema_error: record.name must be a string');
    const raw = r.value;
    const val = typeof raw === 'number' ? raw : Number(String(raw ?? ''));
    if (!Number.isFinite(val)) throw new Error('schema_error: record.value must be finite numeric');
    return [r.name, val];
  });
}

async function listInstances(addr, header, serviceIdStr, duration) {
  const body = await graphql(addr, header, QUERY_INSTANCES, { serviceId: serviceIdStr, duration });
  const rows = pointer(body, '/data/instances');
  if (!Array.isArray(rows)) throw new Error('schema_error: data.instances must be an array');
  return rows.map((r) => {
    if (typeof r?.name !== 'string') throw new Error('schema_error: instance.name must be a string');
    return r.name;
  });
}

async function queryMetrics(addr, header, args) {
  const service = strArg(args, 'service');
  if (!service) {
    throw new Error('schema_error: 指标诊断需要服务名：请在 service 参数里给出要诊断的服务。');
  }
  let minutes = 60;
  if (args?.minutes != null) {
    const m = integerArg(args.minutes);
    if (m !== null && m > 0 && m <= 24 * 60) minutes = m;
  }
  const window = queryWindow(args, 60);
  const available = await listMetricNames(addr, header);
  const can = (name) => !available.length || available.includes(name);
  const windowLabel = strArg(args, 'start')
    ? `${strArg(args, 'start')} 至 ${strArg(args, 'end')}`
    : `最近 ${minutes} 分钟`;
  let out = `SkyWalking 指标诊断（service=${service}，${windowLabel}）：\n`;

  if (can('endpoint_resp_time')) {
    const rows = await topEndpoints(addr, header, 'endpoint_resp_time', service, window.duration());
    if (rows.length) {
      out += '\n【端点响应时间 TopN（ms，降序）】\n';
      for (const [name, val] of rows.slice(0, METRICS_TOPN)) out += `- ${name}: ${val}\n`;
    } else {
      out += '\n【端点响应时间】无数据\n';
    }
  } else {
    out += '\n【端点响应时间】OAP 未提供 endpoint_resp_time 指标\n';
  }

  if (can('endpoint_cpm')) {
    const rows = await topEndpoints(addr, header, 'endpoint_cpm', service, window.duration());
    if (rows.length) {
      out += '\n【端点吞吐 TopN（cpm，降序）】\n';
      for (const [name, val] of rows.slice(0, METRICS_TOPN)) out += `- ${name}: ${val}\n`;
    }
  }

  const sid = await resolveServiceId(addr, header, service);
  const names = await listInstances(addr, header, sid, window.duration());
  if (names.length) {
    out += `\n【服务实例（${names.length} 个）】\n`;
    for (const n of names.slice(0, 30)) out += `- ${n}\n`;
  } else {
    out += '\n【服务实例】无数据\n';
  }

  out += '\n注：以上为相关性证据（延迟/吞吐/实例），不等于已证明的根因。结合日志（传 keyword）与调用链（传 trace_id）定位。';
  return out;
}

// ---------- 工具入口 ----------

export async function skywalkingQuery(args) {
  const addr = ADDR;
  if (!addr) {
    throw new Error('transport_error: 未配置 SkyWalking 地址：请在「设置 → IM机器人 → 企业微信」填写 OAP 地址后再排查线上问题。');
  }
  const kind = strArg(args, 'kind');
  if (kind === 'metrics' || kind === 'diagnose') {
    return queryMetrics(addr, HEADER, args);
  }
  const traceId = strArg(args, 'trace_id');
  if (traceId) {
    // 指定了 traceId 且没给其它日志条件 → 直接拉调用链；否则按日志查（relatedTrace）。
    const onlyTrace = !strArg(args, 'service') && !strArg(args, 'keyword');
    if (onlyTrace && kind !== 'logs') {
      return queryTrace(addr, HEADER, traceId);
    }
  }
  return queryLogs(addr, HEADER, args);
}

// ---------- MCP stdio 协议 ----------

const PROTOCOL_VERSION = '2024-11-05';

const TOOL = {
  name: 'skywalking_query',
  description: 'Query SkyWalking (APM) for production troubleshooting. Three modes, chain them across turns: '
    + '(1) logs — give a service name and/or keyword to search logs in the last `minutes`, with optional '
    + 'exclude_keyword/level and page/page_size; results carry a summary of ERROR-level count and the traceIds '
    + 'worth following (error traces first), each line prefixed with its level and traceId; '
    + '(2) trace — give a trace_id (omit `kind`) to fetch the full call trace rebuilt as a parent/child tree '
    + 'with error-span tags/logs; '
    + '(3) metrics — set kind=metrics with a service to get endpoint response-time/throughput TopN and instance '
    + 'list (correlation evidence, not proven root cause). Recommended flow: search logs → read the ERROR lines '
    + 'and copy their traceId → fetch that trace → optionally metrics, together with reading the code.',
  inputSchema: {
    type: 'object',
    properties: {
      service: { type: 'string', description: 'Service name, optional' },
      endpoint: { type: 'string', description: 'Endpoint name, optional' },
      keyword: { type: 'string', description: 'Keywords to match in log content; pass multiple comma-separated (e.g. 2024102812345,error) and they AND-narrow. Prefer business identifiers from the question: order id, user id, phone, requestId, merchant id.' },
      exclude_keyword: { type: 'string', description: 'Substring to exclude from log content, optional' },
      level: { type: 'string', description: 'Log level tag to filter, e.g. ERROR, optional' },
      trace_id: { type: 'string', description: 'Trace id; alone returns the call trace tree' },
      minutes: { type: 'number', description: 'Look-back window in minutes (default 240 for logs, 60 for metrics)' },
      start: { type: 'string', description: 'Optional explicit RFC3339 start time, paired with end; overrides minutes' },
      end: { type: 'string', description: 'Optional explicit RFC3339 end time; reuse the same fixed range across pages' },
      timezone_offset_minutes: { type: 'number', description: 'OAP fixed UTC offset minutes; defaults to channel setting/local clock' },
      page: { type: 'number', description: 'Log page number, 1-based (default 1)' },
      page_size: { type: 'number', description: 'Logs per page, max 100 (default 40)' },
      kind: { type: 'string', description: 'logs to force log query even with a trace_id; metrics for endpoint/instance diagnosis' },
    },
  },
};

function rpcResult(id, result) {
  return { jsonrpc: '2.0', id, result };
}

function rpcError(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } };
}

async function handleMessage(msg, write) {
  const { id, method, params } = msg || {};
  const isNotification = id === undefined || id === null;
  switch (method) {
    case 'initialize':
      write(rpcResult(id, {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: 'skywalking', version: '1.0.0' },
      }));
      return;
    case 'notifications/initialized':
    case 'notifications/cancelled':
      return;
    case 'ping':
      if (!isNotification) write(rpcResult(id, {}));
      return;
    case 'tools/list':
      write(rpcResult(id, { tools: [TOOL] }));
      return;
    case 'tools/call': {
      if (params?.name !== TOOL.name) {
        write(rpcError(id, -32602, `unknown tool: ${params?.name}`));
        return;
      }
      try {
        const text = await skywalkingQuery(params.arguments || {});
        write(rpcResult(id, { content: [{ type: 'text', text }] }));
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        write(rpcResult(id, { content: [{ type: 'text', text: message }], isError: true }));
      }
      return;
    }
    default:
      if (!isNotification) write(rpcError(id, -32601, `method not found: ${method}`));
  }
}

function main() {
  const rl = createInterface({ input: process.stdin });
  rl.on('line', (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return; // 非 JSON 行直接丢（与宿主侧 mcp.rs 行为一致）
    }
    handleMessage(msg, (out) => {
      process.stdout.write(`${JSON.stringify(out)}\n`);
    }).catch((e) => {
      process.stderr.write(`skywalking-mcp: ${e?.message || e}\n`);
    });
  });
  rl.on('close', () => {
    // 不用 process.exit：等 stdout 写完自然退出，避免管道模式下应答被截断。
  });
}

main();
