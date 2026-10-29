import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
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

  const query = () => {
    const filter: WecomDiagnosticFilter = {};
    for (const key of ["requestId", "phase", "status"] as const) {
      if (draft[key].trim()) filter[key] = draft[key].trim();
    }
    for (const key of ["from", "to"] as const) {
      if (!draft[key]) continue;
      const time = new Date(draft[key]);
      if (!Number.isFinite(time.getTime())) { setError("time"); return; }
      filter[key] = time.toISOString();
    }
    if (filter.from && filter.to && filter.from > filter.to) { setError("time"); return; }
    void load(filter);
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
      <div className="wecom-diag-head"><span>{label("title")}</span><span>{label("newestFirst")}</span></div>
      <p className="setting-hint">{label("hint")}</p>
      <div className="wecom-rows">
        {(["requestId", "phase", "status", "from", "to"] as const).map((key) => (
          <label className="wecom-row" key={key}>
            <span className="wecom-row-label">{label(key)}</span>
            <span className="wecom-row-control">
              <input className="input-field" type={key === "from" || key === "to" ? "datetime-local" : "text"}
                value={draft[key]} onChange={(event) => setDraft((prev) => ({ ...prev, [key]: event.target.value }))} />
            </span>
          </label>
        ))}
      </div>
      <p className="setting-hint">{label("timeHint")}</p>
      <div className="wecom-inline">
        <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={query}>{label("query")}</button>
        <button type="button" className="btn btn-secondary btn-small" disabled={!result || busy} onClick={() => result && void load(result.filter, null, !!selected)}>{t("wecom.diag.refresh")}</button>
        <button type="button" className="btn btn-secondary btn-small" disabled={!result?.events.length || busy || actionBusy} onClick={() => void runAction("copy")}>{label("copy")}</button>
        <button type="button" className="btn btn-secondary btn-small" disabled={!selected || !result || busy || actionBusy} onClick={() => void runAction("export")}>{label("export")}</button>
      </div>
      <p className="setting-hint">{label("privacyHint")}</p>
      {selected ? <p className="setting-hint">{label("selected")} <code>{selected}</code></p> : null}
      {busy ? <p className="setting-hint" role="status">{label("loading")}</p> : null}
      {error ? <p className="setting-hint" role="alert">{label(`${error}Error`)}</p> : null}
      {notice ? <p className="setting-hint" role="status">{label(notice)}</p> : null}
      {!result && !busy && !error ? <p className="setting-hint">{label("idle")}</p> : null}
      {result ? <>
        <p className="setting-hint">{t("wecom.diag.requests.coverage", { count: result.events.length, pages: result.pages, bytes: result.scannedBytes })}</p>
        {result.partial ? <p className="setting-hint" role="status">{label("partial")}</p> : null}
        {result.readErrors.length ? <details className="wecom-hint"><summary>{t("wecom.diag.requests.readErrors", { count: result.readErrors.length })}</summary>
          {result.readErrors.map((code) => <p className="setting-hint" key={code}><code>{code}</code></p>)}
        </details> : null}
        <details className="wecom-hint"><summary>{label("health")}</summary>
          <p className="setting-hint">{label(`healthStatus.${healthStatus}`)}</p>
          <p className="setting-hint">{label("lastSuccessAt")}: <code>{lastSuccessAt ?? t("wecom.diag.notReported")}</code></p>
          {typeof health?.lastError === "string" && health.lastError ? <p className="setting-hint">{label("healthError")} <code>{health.lastError}</code></p> : null}
          {healthEntries.length ? healthEntries.map(({ key, value }) => <p className="setting-hint" key={key}>{label(`healthFields.${key}`)}: <code>{typeof value === "boolean" ? label(value ? "yes" : "no") : value}</code></p>) : <p className="setting-hint">{t("wecom.diag.notReported")}</p>}
        </details>
        {result.events.length === 0 ? <p className="setting-hint">{label("empty")}</p> : (
          <ul className="wecom-diag-sessions" aria-label={label("events")}>
            {result.events.map((event, index) => (
              <li key={`${event.timestamp}-${event.requestId}-${index}`}>
                <details className="wecom-session">
                  <summary><span className="wecom-diag-ts">{event.timestamp}</span><span className="wecom-session-peek">{event.phase} · {event.status} · {event.code ?? "—"}</span></summary>
                  <p className="setting-hint"><code>{event.requestId}</code> · {eventDuration(event)}</p>
                  <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={() => selectRequest(event.requestId)}>{label("select")}</button>
                  <p className="setting-hint">{label("metadata")}</p>
                  <pre className="wecom-diag-log">{JSON.stringify(event.metadata, null, 2).slice(0, 4000)}</pre>
                </details>
              </li>
            ))}
          </ul>
        )}
        {limitReached ? <p className="setting-hint">{label("bounded")}</p> : null}
        <button type="button" className="btn btn-secondary btn-small" disabled={busy || !result.nextCursor || limitReached || error === "cursor"} onClick={() => void load(result.filter, result)}>{label("more")}</button>
        {!result.nextCursor && !limitReached ? <p className="setting-hint">{label("end")}</p> : null}
      </> : null}
    </section>
  );
});
