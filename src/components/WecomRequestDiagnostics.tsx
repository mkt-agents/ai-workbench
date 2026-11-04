import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { SlidersHorizontal } from "lucide-react";
import { useGlobalStore } from "../core/store";
import {
  exportWecomDiagnosticRequest, fetchWecomDiagnosticRequest, fetchWecomDiagnostics,
  wecomDiagnosticSummary,
  type WecomDiagnosticEvent, type WecomDiagnosticFilter, type WecomDiagnosticPage,
} from "../lib/wecomChannel";

const PAGE_SIZE = 50;
const MAX_PAGES = 10;
const MAX_EVENTS = 500;
const EMPTY_DRAFT = { requestId: "", phase: "", status: "", from: "", to: "" };
const HEALTH_FIELDS = ["writeFailures"] as const;

const QUICK_PRESETS = ["m5", "m15", "m30", "h1", "h6", "h24", "d3", "today", "yesterday"] as const;
type QuickPreset = (typeof QUICK_PRESETS)[number];
const QUICK_MINUTES: Record<Exclude<QuickPreset, "today" | "yesterday">, number> = {
  m5: 5, m15: 15, m30: 30, h1: 60, h6: 360, h24: 1440, d3: 4320,
};

const pad2 = (value: number) => String(value).padStart(2, "0");

/** datetime-local 只认「本地时间、不带时区」的字符串，和手填进输入框的格式一致。 */
function toLocalInput(date: Date): string {
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`
    + `T${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}

/** 快捷时间只换算起止两个边界，口径与手填完全相同（仍按本机时区提交）。 */
function quickRange(preset: QuickPreset, now = new Date()): { from: string; to: string } {
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  if (preset === "today") return { from: toLocalInput(midnight), to: toLocalInput(now) };
  if (preset === "yesterday") {
    const start = new Date(midnight);
    start.setDate(start.getDate() - 1);
    // 结束边界取「今天 00:00 之前一分钟」，把昨天整天圈进来又不蹭到今天
    return { from: toLocalInput(start), to: toLocalInput(new Date(midnight.getTime() - 60_000)) };
  }
  const minutes = QUICK_MINUTES[preset];
  return { from: toLocalInput(new Date(now.getTime() - minutes * 60_000)), to: toLocalInput(now) };
}
type Result = WecomDiagnosticPage & { filter: WecomDiagnosticFilter; pages: number; bounded: boolean };
type Props = { selection: { requestId: string; revision: number } | null };

function isStaleCursor(error: unknown): boolean {
  const code = error && typeof error === "object" && "code" in error ? String(error.code) : "";
  return code === "DIAGNOSTICS_CURSOR_STALE" || String(error).includes("DIAGNOSTICS_CURSOR_STALE");
}

/** Separate local drafts/results from the settings card's five-second status polling. */
export default memo(function WecomRequestDiagnostics({ selection }: Props) {
  const { t } = useTranslation("settings");
  const [draft, setDraft] = useState(EMPTY_DRAFT);
  const [preset, setPreset] = useState<QuickPreset | "">("");
  const [result, setResult] = useState<Result | null>(null);
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState(false);
  const [actionBusy, setActionBusy] = useState(false);
  const [error, setError] = useState<"query" | "cursor" | "time" | "action" | "" >("");
  const [notice, setNotice] = useState("");
  const mounted = useRef(false);
  const generation = useRef(0);
  const pending = useRef(false);
  const actionPending = useRef(false);
  const cursors = useRef(new Set<string>());

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current += 1; };
  }, []);

  const load = useCallback(async (filter: WecomDiagnosticFilter, previous: Result | null = null, single = false) => {
    if (previous && pending.current) return;
    const version = ++generation.current;
    pending.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    if (!previous) {
      cursors.current.clear();
      setResult(null);
      setSelected(single ? filter.requestId ?? "" : "");
    }
    try {
      const page = single && filter.requestId
        ? await fetchWecomDiagnosticRequest(filter.requestId)
        : await fetchWecomDiagnostics(filter, previous?.nextCursor ?? undefined, PAGE_SIZE);
      if (!mounted.current || version !== generation.current) return;
      // The backend's descending order is canonical, including ties across pages.
      const events = [...(previous?.events ?? []), ...page.events];
      const repeatedCursor = !!page.nextCursor && cursors.current.has(page.nextCursor);
      if (page.nextCursor) cursors.current.add(page.nextCursor);
      setResult({
        ...page, filter, events: events.slice(0, MAX_EVENTS), pages: (previous?.pages ?? 0) + 1,
        scannedBytes: (previous?.scannedBytes ?? 0) + page.scannedBytes,
        // A previous partial page may only have meant has-more; do not keep it forever.
        partial: page.partial || !!previous?.readErrors.length || !!previous?.bounded,
        readErrors: [...new Set([...(previous?.readErrors ?? []), ...page.readErrors])].slice(0, 20),
        bounded: events.length > MAX_EVENTS || repeatedCursor,
        nextCursor: repeatedCursor ? null : page.nextCursor,
      });
    } catch (cause) {
      if (mounted.current && version === generation.current) {
        const stale = isStaleCursor(cause);
        setError(stale ? "cursor" : "query");
        if (stale) setResult((current) => current ? { ...current, nextCursor: null } : current);
      }
    } finally {
      if (mounted.current && version === generation.current) { pending.current = false; setBusy(false); }
    }
  }, []);

  // This effect only follows an explicit click in the existing session log.
  useEffect(() => {
    if (selection) void load({ requestId: selection.requestId }, null, true);
  }, [selection, load]);

  const query = (source: typeof EMPTY_DRAFT = draft) => {
    const filter: WecomDiagnosticFilter = {};
    for (const key of ["requestId", "phase", "status"] as const) {
      if (source[key].trim()) filter[key] = source[key].trim();
    }
    for (const key of ["from", "to"] as const) {
      if (!source[key]) continue;
      const time = new Date(source[key]);
      if (!Number.isFinite(time.getTime())) { setError("time"); return; }
      filter[key] = time.toISOString();
    }
    if (filter.from && filter.to && filter.from > filter.to) { setError("time"); return; }
    void load(filter);
  };

  /** 点一下快捷时间就落到起止输入框并立即查询：字段同步更新，用户仍能看清实际区间。 */
  const applyQuick = (key: QuickPreset) => {
    const next = { ...draft, ...quickRange(key) };
    setDraft(next);
    setPreset(key);
    query(next);
  };

  /** 一键回到「什么都没查」的状态：筛选条件、结果、选中项全清，在途请求作废。 */
  const resetAll = () => {
    generation.current += 1;
    pending.current = false;
    cursors.current.clear();
    setDraft(EMPTY_DRAFT);
    setPreset("");
    setResult(null);
    setSelected("");
    setBusy(false);
    setError("");
    setNotice("");
  };

  const selectRequest = (requestId: string) => void load({ requestId }, null, true);
  const runAction = async (kind: "copy" | "export") => {
    if (actionPending.current || !result || busy) return;
    const version = generation.current;
    const requestId = selected;
    if (kind === "export" && !requestId) return;
    actionPending.current = true;
    setActionBusy(true);
    setNotice("");
    try {
      const store = useGlobalStore.getState();
      if (kind === "copy") {
        await store.invokeCopyToClipboard(wecomDiagnosticSummary(result.events));
        if (mounted.current && version === generation.current) setNotice("copied");
      } else {
        const content = await exportWecomDiagnosticRequest(requestId);
        // Do not open a late dialog for an unmounted view or a superseded request.
        if (!mounted.current || version !== generation.current) return;
        const filename = `wecom-diagnostics-${requestId.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 80)}.json`;
        const path = await store.invokeSaveTextFile(content, filename, t("wecom.diag.requests.exportTitle"));
        if (mounted.current && version === generation.current) setNotice(path ? "exported" : "exportCancelled");
      }
    } catch {
      if (mounted.current && version === generation.current) setError("action");
    } finally {
      actionPending.current = false;
      if (mounted.current) setActionBusy(false);
    }
  };

  const limitReached = !!result && (result.bounded || result.pages >= MAX_PAGES || result.events.length >= MAX_EVENTS);
  const health = result?.health && typeof result.health === "object" ? result.health as Record<string, unknown> : null;
  const healthEntries = HEALTH_FIELDS.flatMap((key) => {
    const value = health?.[key];
    return typeof value === "boolean" || (typeof value === "number" && Number.isFinite(value) && value >= 0)
      ? [{ key, value }] : [];
  });
  const healthStatus = health?.status === "ok" || health?.status === "degraded" ? health.status : "unknown";
  const lastSuccessAt = typeof health?.lastSuccessAt === "string" && Number.isFinite(Date.parse(health.lastSuccessAt))
    ? new Date(health.lastSuccessAt).toISOString() : null;
  const label = (key: string) => t(`wecom.diag.requests.${key}`);
  const eventDuration = (event: WecomDiagnosticEvent) => typeof event.durationMs === "number" && Number.isFinite(event.durationMs) && event.durationMs >= 0
    ? `${event.durationMs} ms` : t("wecom.diag.notReported");

  return (
    <section className="wecom-diag-block" aria-label={label("title")} aria-busy={busy}>
      <div className="wecom-diag-head">
        <div className="wecom-diag-head-main">
          <span className="wecom-diag-icon" aria-hidden>
            <SlidersHorizontal size={13} />
          </span>
          <span>{label("title")}</span>
        </div>
        <span className="wecom-diag-hint">{label("newestFirst")}</span>
      </div>
      {/* 三条长说明默认收起：筛选行和结果才是这一块的主体，说明不占版面 */}
      <details className="wecom-hint wecom-diag-notes">
        <summary>
          <span>{label("hintShort")}</span>
          <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
        </summary>
        <p className="setting-hint">{label("hint")}</p>
        <p className="setting-hint">{label("timeHint")}</p>
        <p className="setting-hint">{label("privacyHint")}</p>
      </details>
      <div className="wecom-diag-form">
        <div className="wecom-rows is-diag-filter">
          {/* 五个条件共用一套网格：第一行三个精确匹配，第二行起止时间，
              标签列宽固定，跨行跨列的输入框左边界都落在同一条竖线上 */}
          {(["requestId", "phase", "status"] as const).map((key) => (
            <label className="wecom-row" key={key}>
              <span className="wecom-row-label">{label(key)}</span>
              <span className="wecom-row-control">
                <input className="input-field" type="text"
                  value={draft[key]} onChange={(event) => setDraft((prev) => ({ ...prev, [key]: event.target.value }))} />
              </span>
            </label>
          ))}
          {(["from", "to"] as const).map((key) => (
            <label className="wecom-row" key={key}>
              <span className="wecom-row-label">{label(key)}</span>
              <span className="wecom-row-control">
                <input className="input-field" type="datetime-local"
                  value={draft[key]}
                  // 手改了区间就不再是某个快捷档，选中态跟着取消
                  onChange={(event) => { setPreset(""); setDraft((prev) => ({ ...prev, [key]: event.target.value })); }} />
              </span>
            </label>
          ))}
        </div>
        <div className="wecom-inline wecom-diag-actions">
          <div className="wecom-diag-quick" role="group" aria-label={label("quickLabel")}>
            <span className="wecom-diag-quick-label">{label("quickLabel")}</span>
            {QUICK_PRESETS.map((key) => (
              <button key={key} type="button" disabled={busy}
                className={`wecom-diag-quick-chip${preset === key ? " is-active" : ""}`}
                aria-pressed={preset === key}
                onClick={() => applyQuick(key)}>{label(`quick.${key}`)}</button>
            ))}
          </div>
          <div className="wecom-diag-buttons">
            <button type="button" className="btn btn-secondary btn-small" disabled={!result && !selected && !draft.requestId && !draft.phase && !draft.status && !draft.from && !draft.to} onClick={resetAll}>{label("clear")}</button>
            <button type="button" className="btn btn-secondary btn-small" disabled={!result || busy} onClick={() => result && void load(result.filter, null, !!selected)}>{t("wecom.diag.refresh")}</button>
            <button type="button" className="btn btn-secondary btn-small" disabled={!result?.events.length || busy || actionBusy} onClick={() => void runAction("copy")}>{label("copy")}</button>
            <button type="button" className="btn btn-secondary btn-small" disabled={!selected || !result || busy || actionBusy} onClick={() => void runAction("export")}>{label("export")}</button>
            <button type="button" className="btn btn-primary btn-small" disabled={busy} onClick={() => query()}>{label("query")}</button>
          </div>
        </div>
      </div>
      {busy ? <p className="setting-hint" role="status">{label("loading")}</p> : null}
      {error ? <p className="setting-hint" role="alert">{label(`${error}Error`)}</p> : null}
      {notice ? <p className="setting-hint" role="status">{label(notice)}</p> : null}
      {!result && !busy && !error ? <p className="setting-hint">{label("idle")}</p> : null}
      {/* 结果卡：和上面的筛选卡同款边框，块里就是「筛选 → 结果」两段，不再是一片浮着的文字 */}
      {result || selected ? (
        <div className="wecom-diag-result">
          <div className="wecom-diag-strip">
            {selected ? <span className="wecom-diag-pill">{label("selected")}<code>{selected}</code></span> : null}
            {result ? <span className="wecom-chip">{t("wecom.diag.requests.coverage", {
              count: result.events.length, pages: result.pages, bytes: result.scannedBytes.toLocaleString(),
            })}</span> : null}
          </div>
          {result ? <>
            {result.partial ? <p className="wecom-diag-warn" role="status">{label("partial")}</p> : null}
            {result.readErrors.length ? <details className="wecom-hint"><summary>{t("wecom.diag.requests.readErrors", { count: result.readErrors.length })}</summary>
              {result.readErrors.map((code) => <p className="setting-hint" key={code}><code>{code}</code></p>)}
            </details> : null}
            {result.events.length === 0 ? <p className="setting-hint">{label("empty")}</p> : (
              <>
                {/* 列头：一屏几十行时按列扫，比每行混在一句里好读 */}
                <div className="wecom-diag-events-head" aria-hidden="true">
                  <span>{label("columns.time")}</span>
                  <span>{label("columns.phase")}</span>
                  <span>{label("columns.status")}</span>
                  <span>{label("columns.code")}</span>
                </div>
                <ul className="wecom-diag-sessions is-events" aria-label={label("events")}>
                  {result.events.map((event, index) => (
                    <li key={`${event.timestamp}-${event.requestId}-${index}`}>
                      <details className="wecom-session">
                        <summary>
                          <span className="wecom-diag-ts">{event.timestamp}</span>
                          <span className="wecom-event-phase">{event.phase}</span>
                          <span className="wecom-event-status">{event.status}</span>
                          <span className="wecom-event-code">{event.code ?? "—"}</span>
                        </summary>
                        <p className="setting-hint"><code>{event.requestId}</code> · {eventDuration(event)}</p>
                        <button type="button" className="wecom-session-action" disabled={busy} onClick={() => selectRequest(event.requestId)}>{label("select")}</button>
                        <p className="setting-hint">{label("metadata")}</p>
                        <pre className="wecom-diag-log">{JSON.stringify(event.metadata, null, 2).slice(0, 4000)}</pre>
                      </details>
                    </li>
                  ))}
                </ul>
              </>
            )}
            {limitReached ? <p className="setting-hint">{label("bounded")}</p> : null}
            <button type="button" className="wecom-diag-more" disabled={busy || !result.nextCursor || limitReached || error === "cursor"} onClick={() => void load(result.filter, result)}>{label("more")}</button>
            {!result.nextCursor && !limitReached ? <p className="setting-hint">{label("end")}</p> : null}
            {/* 存储健康属于「口径/来源」，压到结果末尾当脚注，别挡在列表前面 */}
            <details className="wecom-hint wecom-diag-storage"><summary>{label("health")}</summary>
              <p className="setting-hint">{label(`healthStatus.${healthStatus}`)}</p>
              <p className="setting-hint">{label("lastSuccessAt")}: <code>{lastSuccessAt ?? t("wecom.diag.notReported")}</code></p>
              {typeof health?.lastError === "string" && health.lastError ? <p className="setting-hint">{label("healthError")} <code>{health.lastError}</code></p> : null}
              {healthEntries.length ? healthEntries.map(({ key, value }) => <p className="setting-hint" key={key}>{label(`healthFields.${key}`)}: <code>{typeof value === "boolean" ? label(value ? "yes" : "no") : value}</code></p>) : <p className="setting-hint">{t("wecom.diag.notReported")}</p>}
            </details>
          </> : null}
        </div>
      ) : null}
    </section>
  );
});
