import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Activity, Gauge, MessagesSquare, Terminal } from "lucide-react";
import type { WecomStatus } from "../lib/wecomChannel";
import {
  fetchWecomLog,
  fetchWecomSessionLogPage,
  type WecomSessionLogPage,
} from "../lib/wecomChannel";
import { reportedCount as countText } from "./wecomBotView";
import WecomRequestDiagnostics from "./WecomRequestDiagnostics";

/** 会话日志时间戳（带偏移的 ISO）→ 本机简写：10-28 11:31。 */
function fmtTs(raw: string): string {
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return raw;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 会话折叠态的一行摘要：取首条有效内容，去掉 markdown 标题井号。 */
function peekLine(text: string): string {
  const line = text
    .split(/\r?\n/)
    .map((s) => s.trim())
    .find(Boolean);
  return (line ?? "").replace(/^#+\s*/, "").slice(0, 160);
}

/**
 * 运行诊断（全通道）。
 *
 * 请求事件、通道日志、会话旁证都是进程级共享数据——所有机器人写进同一份
 * 环形文件，事件本身不区分归属。所以这一页挂在「通道汇总」下，而不是某台
 * 机器人的页签里；页面里不再出现「这是哪台机器人」的错觉。
 */
export default function WecomDiagnosticsPage({ status }: { status: WecomStatus | null }) {
  const { t } = useTranslation("settings");
  const notReported = t("wecom.diag.notReported");
  const performance = status?.lastPerformance;
  // Never merge independently sampled requests or sum parent/child phase snapshots.
  const performanceSummary = performance?.summary?.requestId === performance?.requestId
    ? performance?.summary
    : null;
  // 性能块的 meta 行仍要引用最近一次排障的 requestId / 缓存命中数
  const investigation = status?.lastInvestigation;
  const investigationState = investigation?.status ?? "";
  const decisionGaps = Array.isArray(investigation?.decisionGaps)
    && investigation.decisionGaps.every((gap) => typeof gap === "string")
    ? investigation.decisionGaps
    : null;

  /** 最近快照卡的两个 tab：性能快照 / 排障快照，同一性质的「最近一次」 */
  const [recentTab, setRecentTab] = useState<"perf" | "investigation">("perf");

  const phaseLabel = (phase: string) => phase
    ? t(`wecom.diag.phases.${phase}`, { defaultValue: phase })
    : notReported;
  const reportedCount = (value: number | null | undefined) =>
    countText(value, notReported);
  const reportedMs = (value: number | null | undefined) =>
    typeof value === "number" && Number.isFinite(value) && value >= 0
      ? `${value} ms`
      : notReported;
  // 后端把「本轮没下发 reasoning_effort」记成 default（走服务商默认档），
  // 和真正解析不出值的情况区分开，面板上别都写成「未知」。
  const reportedEffort = (value: string | null | undefined) =>
    !value || value === "default" ? t("wecom.diag.effortDefault") : value;

  const [diagnosticSelection, setDiagnosticSelection] = useState<{ requestId: string; revision: number } | null>(null);
  /** 原始日志长卡里看哪一块：通道日志 / 会话日志 */
  const [logTab, setLogTab] = useState<"channel" | "session">("channel");
  const [logText, setLogText] = useState("");
  const [logBusy, setLogBusy] = useState(false);
  const [sessionPage, setSessionPage] = useState<WecomSessionLogPage | null>(null);
  const sessionLog = sessionPage?.entries ?? [];
  const [sessionError, setSessionError] = useState(false);
  const [sessionBusy, setSessionBusy] = useState(false);
  const mounted = useRef(true);
  const logGeneration = useRef(0);
  const sessionGeneration = useRef(0);
  /** 日志只在第一次挂载时拉一次（空日志也不会反复请求） */
  const loaded = useRef(false);

  useEffect(() => {
    mounted.current = true;
    // 卸载只翻 mounted，不作废在途请求：StrictMode 双挂载（mount → cleanup → 重跑）
    // 会把 generation 顶高一次，而「只拉一次」的旗标又拦住了重发，
    // 回包就会被 generation 拦下、busy 永远不复位（日志卡空白转圈）。
    // 卸载后再 setState 是 React 18 的无害空操作，删掉作废即可。
    return () => {
      mounted.current = false;
    };
  }, []);

  const loadLog = useCallback(async () => {
    const version = ++logGeneration.current;
    setLogBusy(true);
    try {
      const text = await fetchWecomLog(200);
      if (mounted.current && version === logGeneration.current) setLogText(text);
    } catch (e) {
      if (mounted.current && version === logGeneration.current) setLogText(String(e));
    } finally {
      if (mounted.current && version === logGeneration.current) setLogBusy(false);
    }
  }, []);

  const loadSessionLog = useCallback(async () => {
    const version = ++sessionGeneration.current;
    setSessionBusy(true);
    setSessionError(false);
    try {
      const page = await fetchWecomSessionLogPage(30);
      if (mounted.current && version === sessionGeneration.current) setSessionPage(page);
    } catch {
      if (mounted.current && version === sessionGeneration.current) setSessionError(true);
    } finally {
      if (mounted.current && version === sessionGeneration.current) setSessionBusy(false);
    }
  }, []);

  // 进诊断页那一刻才拉日志；拉过一次就不再自动请求（空日志也一样）
  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    void loadLog();
    void loadSessionLog();
  }, [loadLog, loadSessionLog]);

  return (
    <>
      <WecomRequestDiagnostics selection={diagnosticSelection} />
      {/* 最近快照合成一张卡：性能 / 排障同一性质的「最近一次」，tab 切换，全通道视角 */}
      <section className="wecom-diag-block wecom-global-summary">
        <div className="wecom-diag-head">
          <div className="wecom-log-tabs" role="tablist" aria-label={t("wecom.groupDiag")}>
            <button
              type="button"
              role="tab"
              aria-selected={recentTab === "perf"}
              className={`wecom-log-tab${recentTab === "perf" ? " is-active" : ""}`}
              onClick={() => setRecentTab("perf")}
            >
              <Gauge size={13} aria-hidden />
              {t("wecom.diag.performance")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={recentTab === "investigation"}
              className={`wecom-log-tab${recentTab === "investigation" ? " is-active" : ""}`}
              onClick={() => setRecentTab("investigation")}
            >
              <Activity size={13} aria-hidden />
              {t("wecom.diag.latestTitle")}
            </button>
          </div>
          <span className="wecom-global-tag">{t("wecom.scopeAll")}</span>
        </div>
        {recentTab === "perf" ? (
          <>
        {/* 口径说明收起来，块头只留一行快照摘要 */}
        <details className="wecom-hint wecom-diag-notes">
          <summary>
            <span>{t("wecom.diag.performanceHintShort")}</span>
            <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
          </summary>
          <p className="setting-hint">{t("wecom.diag.performanceHint")}</p>
          <p className="setting-hint">{t("wecom.diag.cacheHitsHint")}</p>
        </details>
        {performance ? (
          <>
            <div className="wecom-diag-strip">
              <span className="wecom-diag-pill">
                {t("wecom.diag.performanceRequestId")}<code>{performance.requestId}</code>
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.lastPhase", { phase: phaseLabel(performance.phase) })}
              </span>
              <span className="wecom-chip">
                {t(performance.phase === "total" ? "wecom.diag.totalElapsed" : "wecom.diag.phaseElapsed", {
                  duration: reportedMs(performance.elapsedMs),
                })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.firstEvent", { duration: reportedMs(performance.firstEventMs) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.inputChars", { countText: reportedCount(performance.inputChars) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.outputChars", { countText: reportedCount(performance.outputChars) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.toolEvents", { countText: reportedCount(performance.toolEvents) })}
              </span>
            </div>
            {performanceSummary ? (
              <details className="wecom-hint wecom-diag-phases" key={performanceSummary.requestId}>
                <summary>{t("wecom.diag.phaseTable")}</summary>
                {/* 展开后的明细统一缩进一次，段与段之间 8px，表格不再和文字贴在一起 */}
                <div className="wecom-diag-detail">
                  <p className="setting-hint">
                    {t("wecom.diag.performanceRequestId")} <code>{performanceSummary.requestId}</code>
                  </p>
                  <p className="setting-hint">{t("wecom.diag.phaseTableHint")}</p>
                  <div className="wecom-diag-strip">
                    <span className="wecom-chip">{t("wecom.diag.totalElapsed", { duration: reportedMs(performanceSummary.totalMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.queueElapsed", { duration: reportedMs(performanceSummary.queueMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.elapsedWithQueue", { duration: reportedMs(performanceSummary.elapsedWithQueueMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.sessionAction", {
                      action: performanceSummary.sessionAction
                        ? t(`wecom.diag.sessionActions.${performanceSummary.sessionAction}`, { defaultValue: performanceSummary.sessionAction })
                        : notReported,
                    })}</span>
                  </div>
                  <p className="setting-hint">{t("wecom.diag.modelReported", { model: performanceSummary.model || notReported, effort: reportedEffort(performanceSummary.effort) })}</p>
                  {Array.isArray(performanceSummary.phases) && performanceSummary.phases.length > 0 ? (
                    <div className="wecom-diag-tablebox" tabIndex={0} role="region" aria-label={t("wecom.diag.phaseTable")}>
                      <table className="wecom-diag-table">
                        <thead><tr>
                          {(["phase", "elapsed", "calls", "input", "output", "tools", "first"] as const).map((column) => (
                            <th scope="col" key={column}>{t(`wecom.diag.columns.${column}`)}</th>
                          ))}
                        </tr></thead>
                        <tbody>{performanceSummary.phases.map((row, index) => (
                          <tr key={`${row.phase}-${index}`} className={row.phase === "total" ? "is-total" : undefined}>
                            <th scope="row">{phaseLabel(row.phase)} <code>{row.phase}</code></th>
                            <td>{reportedMs(row.elapsedMs)}</td>
                            <td>{reportedCount(row.calls)}</td>
                            <td>{reportedCount(row.inputChars)}</td>
                            <td>{reportedCount(row.outputChars)}</td>
                            <td>{reportedCount(row.toolEvents)}</td>
                            <td>{reportedMs(row.firstEventMs)}</td>
                          </tr>
                        ))}</tbody>
                      </table>
                    </div>
                  ) : <p className="setting-hint">{notReported}</p>}
                  <p className="setting-hint">{t("wecom.diag.validationFailures", {
                    countText: typeof performanceSummary.validationFailureCount === "number"
                      && Number.isFinite(performanceSummary.validationFailureCount) && performanceSummary.validationFailureCount >= 0
                      ? reportedCount(performanceSummary.validationFailureCount)
                      : t("wecom.diag.validationNotReported"),
                  })}</p>
                  {performanceSummary.validationFailures?.map((failure, index) => (
                    <p className="setting-hint" key={`${index}-${failure}`}>{failure}</p>
                  ))}
                </div>
              </details>
            ) : null}
          </>
        ) : (
          // 空快照不是「数据丢了」：这一块只记当前进程里最后完成的一轮，
          // 启动后还没答过消息（或刚重启过）就会是空的，所以直接说明原因
          <div className="wecom-empty-state">
            <span className="wecom-empty-state-icon" aria-hidden>
              <Gauge size={18} />
            </span>
            <p>{t("wecom.diag.performanceEmpty")}</p>
          </div>
        )}
        {/* 没有任何快照时不重复一行「未提供」：上面已经把原因说清楚了 */}
        {performance || investigation ? (
          <p className="setting-hint wecom-diag-meta">
            {t("wecom.diag.cacheHits", { countText: reportedCount(investigation?.cacheHits) })}
            {investigation?.requestId ? <> · {t("wecom.diag.requestId")} <code>{investigation.requestId}</code></> : null}
          </p>
        ) : null}
          </>
        ) : (
          <>
            <div className="wecom-facts">
              <div className="wecom-fact">
                <span className="wecom-fact-label">{t("wecom.diag.factTroubleshoot")}</span>
                <span className="wecom-fact-items">
                  <span
                    className={`wecom-status-pill${
                      ["source_error", "source_failed", "error"].includes(investigationState)
                        ? " is-error"
                        : ""
                    }`}
                  >
                    <i className="wecom-status-dot" aria-hidden />
                    {investigation
                      ? t(`wecom.diag.investigationState.${investigationState}`, {
                          defaultValue: investigationState,
                        })
                      : t("wecom.diag.noInvestigation")}
                  </span>
                  {investigation ? (
                    <>
                      <span className="wecom-chip">
                        {t("wecom.diag.actualRequests", {
                          countText: reportedCount(investigation.requestCount),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.evidenceCount", {
                          countText: reportedCount(investigation.evidenceCount),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.warnCount", {
                          countText: reportedCount(investigation.warnCount),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.truncatedCount", {
                          countText: reportedCount(investigation.truncatedCount),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.incompletePages", {
                          countText: reportedCount(investigation.incompletePages),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.source", {
                          state:
                            investigation.sourceReady === true
                              ? t("wecom.diag.sourceReady")
                              : investigation.sourceReady === false
                                ? t("wecom.diag.sourceNotReady")
                                : t("wecom.diag.notReported"),
                        })}
                      </span>
                    </>
                  ) : null}
                </span>
              </div>
              <div className="wecom-fact">
                <span className="wecom-fact-label">{t("wecom.diag.factCrossQuery")}</span>
                <span className="wecom-fact-items">
                  {status?.lastCrossQuery ? (
                    <>
                      <span className="wecom-chip">
                        {t("wecom.diag.crossDocs", {
                          countText: reportedCount(status.lastCrossQuery.docHits),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.crossCode", {
                          countText: reportedCount(status.lastCrossQuery.codeHits),
                        })}
                      </span>
                      <span className="wecom-chip">
                        {t("wecom.diag.crossLogs", {
                          countText: reportedCount(status.lastCrossQuery.logHits),
                        })}
                      </span>
                      <span className="wecom-chip" title={status.lastCrossQuery.summary}>
                        {status.lastCrossQuery.engine}
                      </span>
                    </>
                  ) : (
                    <span className="wecom-chip">{t("wecom.diag.noCrossQuery")}</span>
                  )}
                </span>
              </div>
            </div>
            {status?.lastError ? (
              <p className="wecom-hero-error" title={status.lastError}>
                {t("wecom.diag.lastError", { error: status.lastError })}
              </p>
            ) : null}
            <details className="wecom-hint wecom-diag-notes">
              <summary>
                <span>{t("wecom.hintShort")}</span>
                <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
              </summary>
              <p className="setting-hint">{t("wecom.diag.jobsHint")}</p>
              <p className="setting-hint" title={t("wecom.diag.instanceHint")}>
                {t("wecom.diag.instance", {
                  pid: status?.processId ?? notReported,
                  build: status?.buildLabel || notReported,
                })}
              </p>
              {investigation?.requestId ? (
                <p className="setting-hint">
                  {t("wecom.diag.requestId")} <code>{investigation.requestId}</code>
                </p>
              ) : null}
              {investigation?.detail ? <p className="setting-hint">{investigation.detail}</p> : null}
              {investigation?.clarification ? (
                <p className="setting-hint" role="alert">
                  {investigation.clarification}
                </p>
              ) : null}
              {investigation ? (
                <>
                  <p className="setting-hint">
                    {t("wecom.diag.decisionGaps", {
                      countText:
                        decisionGaps === null
                          ? notReported
                          : reportedCount(decisionGaps.length),
                    })}
                  </p>
                  {decisionGaps?.map((gap, index) => (
                    <p className="setting-hint" key={`${index}-${gap}`}>
                      {gap}
                    </p>
                  ))}
                </>
              ) : null}
              <p className="setting-hint">{t("wecom.diag.countHint")}</p>
            </details>
          </>
        )}
      </section>
      {/* 两块日志降一层：分组标题保留；内容合成一张长卡，tab 切换看哪块，
          刷新只刷当前 tab，数据仍在进诊断页时一次拉齐 */}
      <div className="wecom-subhead wecom-diag-subhead">{t("wecom.diag.rawLogs")}</div>
      <section className="wecom-diag-block wecom-diag-logs">
        <div className="wecom-diag-head">
          <div className="wecom-log-tabs" role="tablist" aria-label={t("wecom.diag.rawLogs")}>
            <button
              type="button"
              role="tab"
              aria-selected={logTab === "channel"}
              className={`wecom-log-tab${logTab === "channel" ? " is-active" : ""}`}
              onClick={() => setLogTab("channel")}
            >
              <Terminal size={13} aria-hidden />
              {t("wecom.diag.channelLog")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={logTab === "session"}
              className={`wecom-log-tab${logTab === "session" ? " is-active" : ""}`}
              onClick={() => setLogTab("session")}
            >
              <MessagesSquare size={13} aria-hidden />
              {t("wecom.diag.sessionLog")}
            </button>
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={logTab === "channel" ? logBusy : sessionBusy}
            onClick={() => void (logTab === "channel" ? loadLog() : loadSessionLog())}
          >
            {t("wecom.diag.refresh")}
          </button>
        </div>
        {logTab === "channel" ? (
          <>
            <p className="setting-hint">{t("wecom.diag.logHint")}</p>
            <pre className="wecom-diag-log">{logText || (logBusy ? "…" : t("wecom.diag.empty"))}</pre>
          </>
        ) : (
          <>
            <p className="setting-hint">{t("wecom.diag.sessionHint")}</p>
            {/* 会话旁证同样套一张结果卡：和请求事件、日志块的边框对齐，不再是一列浮在底色上的行 */}
            <div className="wecom-diag-result">
              {sessionError ? <p className="setting-hint" role="alert">{t("wecom.diag.sessionReadFailed")}</p> : null}
              {sessionPage ? <>
                <div className="wecom-diag-strip">
                  <span className="wecom-chip">{t("wecom.diag.sessionCoverage", {
                    count: sessionPage.entries.length, bytes: sessionPage.scannedBytes.toLocaleString(),
                  })}</span>
                </div>
                {sessionPage.partial ? <p className="wecom-diag-warn" role="status">{t("wecom.diag.sessionPartial")}</p> : null}
                {sessionPage.readErrors.length > 0 ? <details className="wecom-hint">
                  <summary>{t("wecom.diag.sessionReadErrors", { count: sessionPage.readErrors.length })}</summary>
                  {sessionPage.readErrors.map((code, index) => <p className="setting-hint" key={`${index}-${code}`}><code>{code}</code></p>)}
                </details> : null}
              </> : null}
              {sessionLog.length === 0 ? (
                !sessionError ? <p className="setting-hint">{sessionBusy ? "…" : t("wecom.diag.empty")}</p> : null
              ) : (
                <ul className="wecom-diag-sessions">
                  {sessionLog.map((entry, i) => (
                    <li key={`${entry.session}-${entry.ts}-${i}`}>
                      <details className="wecom-session">
                        <summary>
                          <span className="wecom-diag-ts">{fmtTs(entry.ts)}</span>
                          <span className="wecom-diag-u">{entry.user}</span>
                          <span className="wecom-session-peek">{peekLine(entry.assistant)}</span>
                        </summary>
                        <p className="wecom-session-full">{entry.assistant}</p>
                      </details>
                      {entry.requestId ? (
                        <button type="button" className="wecom-session-action"
                          onClick={() => setDiagnosticSelection((previous) => ({ requestId: entry.requestId!, revision: (previous?.revision ?? 0) + 1 }))}>
                          {t("wecom.diag.requests.select")} <code>{entry.requestId}</code>
                        </button>
                      ) : null}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        )}
      </section>
    </>
  );
}
