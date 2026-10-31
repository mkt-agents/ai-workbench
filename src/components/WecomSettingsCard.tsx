import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff } from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import GroupedSelect from "./GroupedSelect";
import WecomRequestDiagnostics from "./WecomRequestDiagnostics";
import { useGlobalStore } from "../core/store";
import type { AppSettings, DshModelCatalog } from "../core/types";
import {
  applyWecomChannel,
  protectSecret,
  revealWecomSecret,
  approveWecomPairing,
  fetchDshModels,
  fetchWecomLog,
  fetchWecomPairings,
  fetchWecomSessionLogPage,
  fetchWecomStatus,
  normalizeWecomMaxJobs,
  restartWecomChannel,
  revokeWecomUser,
  runWecomDocgenNow,
  syncDshProfile,
  testWecomDocgen,
  type WecomPairing,
  type WecomSessionLogPage,
  type WecomStatus,
} from "../lib/wecomChannel";

const MAX_JOBS_CHOICES = [1, 2, 4, 8, 12, 16, 24, 32];

/** Controlled text that follows persisted settings unless the field is focused. */
function useFieldDraft(saved: string) {
  const [value, setValue] = useState(saved);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current) setValue(saved);
  }, [saved]);
  return { value, setValue, focused };
}

function parseList(raw: string): string[] {
  return raw
    .split(/[,;\n\r]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function statusClass(state: string | undefined): string {
  if (state === "connected") return "is-connected";
  if (state === "error") return "is-error";
  if (state === "connecting" || state === "reconnecting") return "is-pending";
  return "";
}

/** 已连接时长（ms）→ 简短中文：如 2小时3分、5分12秒、38秒。 */
function fmtDuration(ms: number | null | undefined): string {
  if (ms == null || ms < 0) return "";
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h${m}m`;
  if (m > 0) return `${m}m${sec}s`;
  return `${sec}s`;
}

/** 镜像时间 → 当天只显示时刻（20:11），跨天带日期（10-28 20:11）。 */
function fmtStamp(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return sameDay ? hm : `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
}

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

/** DSH 子状态：ready / starting / error 复用同一套胶囊配色。 */
function dshStateClass(state: string | undefined): string {
  if (state === "ready") return "is-connected";
  if (state === "error") return "is-error";
  if (state === "starting") return "is-pending";
  return "";
}

/**
 * 字段说明：默认只露一句摘要，长文案收进「说明 ⌄」。
 * 这一页原来是被整段说明撑乱的，宁可多一次点击也不要一堵墙。
 */
function Hint({ summary, details }: { summary: string; details?: string }) {
  const { t } = useTranslation("settings");
  if (!details) return <p className="setting-hint">{summary}</p>;
  return (
    <details className="wecom-hint">
      <summary>
        <span>{summary}</span>
        <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
      </summary>
      <p className="setting-hint">{details}</p>
    </details>
  );
}

/**
 * 一行：字段名在上、控件在下（说明另起一行）；`wide` 让长字段横跨两列。
 * `asSwitch` 换成「标签+说明在左、开关在右」的整行开关，布尔项不再孤零零占半个格子。
 */
function Row({
  label,
  control,
  hint,
  wide,
  asSwitch,
}: {
  label: string;
  control: React.ReactNode;
  hint?: React.ReactNode;
  wide?: boolean;
  asSwitch?: boolean;
}) {
  if (asSwitch) {
    return (
      <div className="wecom-row wecom-switch-row">
        <div className="wecom-switch-text">
          <span className="wecom-row-label">{label}</span>
          {hint ? <div className="wecom-row-hint">{hint}</div> : null}
        </div>
        <div className="wecom-switch-control">{control}</div>
      </div>
    );
  }
  return (
    <div className={`wecom-row${wide ? " is-wide" : ""}`}>
      <span className="wecom-row-label">{label}</span>
      <div className="wecom-row-control">{control}</div>
      {hint ? <div className="wecom-row-hint">{hint}</div> : null}
    </div>
  );
}

/** 页面内部的小卡片 Tab：点谁只看谁，一屏放得下。 */
type WecomSection = "channel" | "answer" | "trouble" | "autoupdate" | "access" | "diag";

/** 扁平分区标题：与参考图一致，不使用图标卡片；右侧可挂开关 / 计数。 */
function PanelHead({ title, aside }: { title: string; aside?: React.ReactNode }) {
  return (
    <div className="wecom-panel-head">
      <div className="hub-group-title">{title}</div>
      {aside ? <div className="wecom-panel-aside">{aside}</div> : null}
    </div>
  );
}

export default function WecomSettingsCard() {
  const { t } = useTranslation("settings");
  const confirm = useConfirm();
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const mcpServers = useGlobalStore((s) => s.mcpServers);

  const [status, setStatus] = useState<WecomStatus | null>(null);
  const [pairings, setPairings] = useState<WecomPairing[]>([]);
  const [secretDraft, setSecretDraft] = useState("");
  const [revealedSecret, setRevealedSecret] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [corpSecretDraft, setCorpSecretDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [docgenBusy, setDocgenBusy] = useState(false);
  // DSH 引擎的模型目录（ACP configOptions），按引擎就绪懒加载
  const [catalog, setCatalog] = useState<DshModelCatalog | null>(null);
  const [catalogBusy, setCatalogBusy] = useState(false);
  const [catalogError, setCatalogError] = useState("");
  // 排障折叠区：通道日志尾 + 最近会话
  const [logText, setLogText] = useState("");
  const [logBusy, setLogBusy] = useState(false);
  const [sessionPage, setSessionPage] = useState<WecomSessionLogPage | null>(null);
  const sessionLog = sessionPage?.entries ?? [];
  const [sessionError, setSessionError] = useState(false);
  const [sessionBusy, setSessionBusy] = useState(false);
  const [diagnosticSelection, setDiagnosticSelection] = useState<{ requestId: string; revision: number } | null>(null);
  /** 小卡片 Tab：一页只显示一组设置，别把六七块叠成一条长滚动 */
  const [section, setSection] = useState<WecomSection>("channel");
  /** 诊断日志只在第一次切过去时拉一次（空日志也不会反复请求） */
  const diagLoaded = useRef(false);
  const mounted = useRef(false);
  const refreshGeneration = useRef(0);
  const logGeneration = useRef(0);
  const sessionGeneration = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      refreshGeneration.current += 1;
      logGeneration.current += 1;
      sessionGeneration.current += 1;
    };
  }, []);

  const botId = useFieldDraft(settings.wecomBotId ?? "");
  const corpId = useFieldDraft(settings.wecomCorpId ?? "");
  const displayName = useFieldDraft(settings.wecomBotDisplayName ?? "");
  const allowUsers = useFieldDraft((settings.wecomAllowUsers ?? []).join(", "));
  const allowGroups = useFieldDraft((settings.wecomAllowGroups ?? []).join(", "));
  const repoPath = useFieldDraft(settings.wecomRepoPath ?? "");
  const docDir = useFieldDraft(settings.wecomDocDir ?? "");
  const selfIntro = useFieldDraft(settings.wecomSelfIntro ?? "");
  const skywalkingAddr = useFieldDraft(settings.wecomSkywalkingAddr ?? "");
  const skywalkingHeader = useFieldDraft(settings.wecomSkywalkingHeader ?? "");
  const releaseBranch = useFieldDraft(settings.wecomReleaseBranch ?? "");
  const autoUpdateTime = useFieldDraft(settings.wecomAutoUpdateTime ?? "");
  const [webhookDraft, setWebhookDraft] = useState("");

  const refresh = useCallback(async () => {
    const version = ++refreshGeneration.current;
    try {
      const [st, pairs] = await Promise.all([fetchWecomStatus(), fetchWecomPairings()]);
      if (!mounted.current || version !== refreshGeneration.current) return;
      setStatus(st);
      setPairings(pairs);
    } catch (e) {
      if (mounted.current && version === refreshGeneration.current) setMsg(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const push = async (
    next: Partial<AppSettings> = {},
    plainSecret?: string,
    plainCorpSecret?: string,
  ) => {
    setBusy(true);
    setMsg("");
    const enabling = next.wecomEnabled === true;
    try {
      let patch: Partial<AppSettings> = { ...next };
      const plain = plainSecret?.trim() ?? "";
      if (plain) {
        patch = { ...patch, wecomBotSecret: await protectSecret(plain) };
      }
      const plainCorp = plainCorpSecret?.trim() ?? "";
      if (plainCorp) {
        patch = { ...patch, wecomCorpSecret: await protectSecret(plainCorp) };
      }
      if (Object.keys(patch).length) setSettings(patch);
      const merged = { ...useGlobalStore.getState().settings, ...patch };
      const st = await applyWecomChannel({
        settings: merged,
        models: aiModels,
        mcpServers,
        plainSecret: plain || undefined,
        plainCorpSecret: plainCorp || undefined,
      });
      setStatus(st);
      if (plain) {
        setSecretDraft("");
        setRevealedSecret("");
        setShowSecret(false);
      }
      if (plainCorp) setCorpSecretDraft("");
      await refresh();
      setMsg(t("wecom.saved"));
    } catch (e) {
      if (enabling) setSettings({ wecomEnabled: false });
      setMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleSecret = async () => {
    if (showSecret) {
      setShowSecret(false);
      if (secretDraft.trim() && secretDraft.trim() === revealedSecret.trim()) {
        setSecretDraft("");
      }
      return;
    }
    if (!secretDraft.trim() && settings.wecomBotSecret) {
      try {
        const plain = await revealWecomSecret(settings.wecomBotSecret);
        setRevealedSecret(plain);
        setSecretDraft(plain);
      } catch (e) {
        setMsg(String(e));
        return;
      }
    }
    setShowSecret(true);
  };

  const stateKey = status?.state ?? "disconnected";
  const stateLabel = t(`wecom.state.${stateKey}`, { defaultValue: stateKey });
  const engine = settings.wecomEngine === "dsh" ? "dsh" : "tianji";
  const dshState = status?.dsh?.state ?? "stopped";
  const dshLabel = t(`wecom.dsh.state.${dshState}`, { defaultValue: dshState });
  const dshReady = dshState === "ready";

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

  // 切到「运行诊断」那一刻才拉日志；拉过一次就不再自动请求（空日志也一样）
  useEffect(() => {
    if (section !== "diag" || diagLoaded.current) return;
    diagLoaded.current = true;
    void loadLog();
    void loadSessionLog();
  }, [section, loadLog, loadSessionLog]);

  const mirror = status?.dsh?.mirror ?? null;

  const loadModels = useCallback(async (refresh: boolean) => {
    setCatalogBusy(true);
    setCatalogError("");
    try {
      setCatalog(await fetchDshModels(refresh));
    } catch (e) {
      setCatalogError(String(e));
    } finally {
      setCatalogBusy(false);
    }
  }, []);

  // 引擎就绪后再拉模型目录；DSH 没起来时不主动去启进程。
  useEffect(() => {
    if (engine !== "dsh" || !dshReady || catalog) return;
    void loadModels(false);
  }, [engine, dshReady, catalog, loadModels]);

  const resyncProfile = async () => {
    setBusy(true);
    setMsg("");
    try {
      await syncDshProfile();
      // 镜像后引擎会重启，旧目录作废，等就绪再拉一次。
      setCatalog(null);
      await refresh();
      setMsg(t("wecom.dshResynced"));
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  const performance = status?.lastPerformance;
  // Never merge independently sampled requests or sum parent/child phase snapshots.
  const performanceSummary = performance?.summary?.requestId === performance?.requestId
    ? performance?.summary
    : null;
  const phaseLabel = (phase: string) => phase
    ? t(`wecom.diag.phases.${phase}`, { defaultValue: phase })
    : t("wecom.diag.notReported");
  const investigation = status?.lastInvestigation;
  const investigationState = investigation?.status ?? "";
  const decisionGaps = Array.isArray(investigation?.decisionGaps)
    && investigation.decisionGaps.every((gap) => typeof gap === "string")
    ? investigation.decisionGaps
    : null;
  const reportedCount = (value: number | null | undefined) =>
    typeof value === "number" && Number.isFinite(value) && value >= 0
      ? String(value)
      : t("wecom.diag.notReported");

  const reportedMs = (value: number | null | undefined) =>
    typeof value === "number" && Number.isFinite(value) && value >= 0
      ? `${value} ms`
      : t("wecom.diag.notReported");

  const maxJobs = normalizeWecomMaxJobs(settings.wecomMaxJobs);
  // 保留已保存的 3/6 或其它合法值，避免原生 select 无匹配项而显示错误档位。
  const maxJobsChoices = MAX_JOBS_CHOICES.includes(maxJobs)
    ? MAX_JOBS_CHOICES
    : [...MAX_JOBS_CHOICES, maxJobs].sort((a, b) => a - b);
  const modelValue = settings.wecomDshModel ?? "";
  const modelCount =
    catalog?.groups.reduce((total, group) => total + group.options.length, 0) ?? 0;

  return (
    <div className="wecom-settings">
      {/* 概览条：同类信息挤一行（连接 / 引擎 / 时长 / 最近排障 / 计数），长说明收进「说明」 */}
      <div className="wecom-hero">
        <div className="wecom-hero-main">
          <span
            className={`wecom-status-pill ${statusClass(status?.state)}`}
            title={status?.detail || undefined}
          >
            <i className="wecom-status-dot" aria-hidden />
            {stateLabel}
          </span>
          <span className="wecom-hero-tag">
            {engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji")}
          </span>
          {status?.connectedMs != null ? (
            <span className="wecom-chip">
              {t("wecom.diag.connectedFor", { duration: fmtDuration(status.connectedMs) })}
            </span>
          ) : null}
        </div>
        <div className="wecom-hero-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy || !settings.wecomEnabled}
            onClick={() => {
              setBusy(true);
              void restartWecomChannel()
                .then((st) => {
                  setStatus(st);
                  setMsg(t("wecom.restarted"));
                })
                .catch((e) => setMsg(String(e)))
                .finally(() => setBusy(false));
            }}
          >
            {t("wecom.restart")}
          </button>
          <label className="toggle" title={t("wecom.enabled")}>
            <input
              type="checkbox"
              checked={Boolean(settings.wecomEnabled)}
              disabled={busy}
              onChange={(e) => void push({ wecomEnabled: e.target.checked })}
            />
            <span className="toggle-slider" />
          </label>
        </div>
        {/* 计数按用途归两行：通道运行态 / 最近一次排障的采样结果 */}
        <div className="wecom-facts">
          <div className="wecom-fact">
            <span className="wecom-fact-label">{t("wecom.groupChannel")}</span>
            <span className="wecom-fact-items">
              <span className="wecom-chip">
                {t("wecom.diag.activeJobs", { countText: reportedCount(status?.activeJobs) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.maxJobs", { countText: reportedCount(status?.maxJobs) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.waitingJobs", { countText: reportedCount(status?.waitingJobs) })}
              </span>
              <span className="wecom-chip">
                {t("wecom.diag.sharedTasks", { countText: reportedCount(status?.sharedTasks) })}
              </span>
            </span>
          </div>
          <div className="wecom-fact">
            <span className="wecom-fact-label">{t("wecom.diag.factTroubleshoot")}</span>
            <span className="wecom-fact-items">
              <span
                className={`wecom-status-pill${
                  ["source_error", "source_failed", "error"].includes(investigationState) ? " is-error" : ""
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
                    {t("wecom.diag.actualRequests", { countText: reportedCount(investigation.requestCount) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.evidenceCount", { countText: reportedCount(investigation.evidenceCount) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.shownCount", { countText: reportedCount(investigation.shownCount) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.warnCount", { countText: reportedCount(investigation.warnCount) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.truncatedCount", { countText: reportedCount(investigation.truncatedCount) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.incompletePages", { countText: reportedCount(investigation.incompletePages) })}
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
        </div>
        {status?.lastError ? (
          <p className="wecom-hero-error" title={status.lastError}>
            {t("wecom.diag.lastError", { error: status.lastError })}
          </p>
        ) : null}
        {/* 实例 / requestId / 计数口径这些长文案默认收起，概览条只留一行状态 */}
        <details className="wecom-hint wecom-hero-hint">
          <summary>
            <span>{t("wecom.hintShort")}</span>
            <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
          </summary>
          <p className="setting-hint">{t("wecom.hint")}</p>
          <p className="setting-hint">{t("wecom.diag.jobsHint")}</p>
          {status?.detail ? <p className="setting-hint">{status.detail}</p> : null}
          <p className="setting-hint" title={t("wecom.diag.instanceHint")}>
            {t("wecom.diag.instance", {
              pid: status?.processId ?? t("wecom.diag.notReported"),
              build: status?.buildLabel || t("wecom.diag.notReported"),
            })}
          </p>
          {investigation?.requestId ? (
            <p className="setting-hint">
              {t("wecom.diag.requestId")} <code>{investigation.requestId}</code>
            </p>
          ) : null}
          {investigation?.detail ? <p className="setting-hint">{investigation.detail}</p> : null}
          {investigation?.clarification ? (
            <p className="setting-hint" role="alert">{investigation.clarification}</p>
          ) : null}
          {investigation ? (
            <>
              <p className="setting-hint">
                {t("wecom.diag.decisionGaps", {
                  countText: decisionGaps === null
                    ? t("wecom.diag.notReported")
                    : reportedCount(decisionGaps.length),
                })}
              </p>
              {decisionGaps?.map((gap, index) => (
                <p className="setting-hint" key={`${index}-${gap}`}>{gap}</p>
              ))}
            </>
          ) : null}
          <p className="setting-hint">{t("wecom.diag.countHint")}</p>
        </details>
      </div>

      {/* 小卡片 Tab：一屏只看一组设置，不再把六七块叠成一条长滚动 */}
      <div className="wecom-sections" role="tablist" aria-label={t("wecom.title")}>
        {(
          [
            {
              id: "channel",
              title: t("wecom.groupChannel"),
              summary: settings.wecomEnabled ? stateLabel : t("wecom.sectionOff"),
            },
            {
              id: "answer",
              title: t("wecom.groupAnswer"),
              summary: engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji"),
            },
            {
              id: "trouble",
              title: t("wecom.groupTroubleshoot"),
              summary: settings.wecomSkywalkingAddr?.trim() || t("wecom.sectionUnset"),
            },
            {
              id: "autoupdate",
              title: t("wecom.groupAutoupdate"),
              summary: settings.wecomAutoUpdateEnabled
                ? t("wecom.sectionDailyAt", { time: settings.wecomAutoUpdateTime || "23:00" })
                : t("wecom.sectionOff"),
            },
            {
              id: "access",
              title: t("wecom.groupAccess"),
              summary: t("wecom.sectionPaired", { count: (settings.wecomPairedUsers ?? []).length }),
            },
            {
              id: "diag",
              title: t("wecom.groupDiag"),
              summary: t("wecom.sectionLogs"),
            },
          ] as const
        ).map((item) => (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={section === item.id}
            className={`wecom-section${section === item.id ? " is-active" : ""}`}
            onClick={() => setSection(item.id)}
          >
            <span className="wecom-section-title">{item.title}</span>
            <span className="wecom-section-summary">{item.summary}</span>
          </button>
        ))}
      </div>

      {/* 1 · 通道与凭据：开了没、连上没、用哪个机器人 */}
      <div className="net-panel" hidden={section !== "channel"}>
        <PanelHead title={t("wecom.groupChannel")} />
        <div className="wecom-rows">
          <div className="wecom-subhead">{t("wecom.subBot")}</div>

          <Row
            wide
            label={t("wecom.displayName")}
            control={
              <input
                className="input-field"
                value={displayName.value}
                placeholder={t("wecom.displayNamePlaceholder")}
                onFocus={() => {
                  displayName.focused.current = true;
                }}
                onChange={(e) => displayName.setValue(e.target.value)}
                onBlur={() => {
                  displayName.focused.current = false;
                  const v = displayName.value.trim();
                  if (v !== (settings.wecomBotDisplayName ?? "")) void push({ wecomBotDisplayName: v });
                }}
              />
            }
          />

          <Row
            label={t("wecom.botId")}
            control={
              <input
                className="input-field"
                value={botId.value}
                placeholder="botId"
                onFocus={() => {
                  botId.focused.current = true;
                }}
                onChange={(e) => botId.setValue(e.target.value)}
                onBlur={() => {
                  botId.focused.current = false;
                  const v = botId.value.trim();
                  if (v !== (settings.wecomBotId ?? "")) void push({ wecomBotId: v });
                }}
              />
            }
          />

          <Row
            label={t("wecom.botSecret")}
            control={
              <div className="input-with-toggle">
                <input
                  className="input-field"
                  type={showSecret ? "text" : "password"}
                  value={showSecret ? secretDraft || revealedSecret : secretDraft}
                  placeholder={
                    settings.wecomBotSecret
                      ? showSecret
                        ? ""
                        : `••••••••  ${t("wecom.secretKept")}`
                      : t("wecom.secretPlaceholder")
                  }
                  onChange={(e) => setSecretDraft(e.target.value)}
                  onBlur={() => {
                    const v = secretDraft.trim();
                    if (!v || v === revealedSecret.trim()) {
                      if (!showSecret || v === revealedSecret.trim()) setSecretDraft("");
                      return;
                    }
                    void push({}, v);
                  }}
                />
                <button
                  type="button"
                  className="input-toggle-btn"
                  title={showSecret ? t("wecom.hideSecret") : t("wecom.showSecret")}
                  aria-pressed={showSecret}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => void toggleSecret()}
                >
                  {showSecret ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              </div>
            }
          />

          <div className="wecom-subhead">{t("wecom.subCorp")}</div>

          <Row
            label={t("wecom.corpId")}
            control={
              <input
                className="input-field"
                value={corpId.value}
                placeholder={t("wecom.corpIdPlaceholder")}
                onFocus={() => {
                  corpId.focused.current = true;
                }}
                onChange={(e) => corpId.setValue(e.target.value)}
                onBlur={() => {
                  corpId.focused.current = false;
                  const v = corpId.value.trim();
                  if (v !== (settings.wecomCorpId ?? "")) void push({ wecomCorpId: v });
                }}
              />
            }
          />

          <Row
            label={t("wecom.corpSecret")}
            control={
              <input
                className="input-field"
                type="password"
                value={corpSecretDraft}
                placeholder={
                  settings.wecomCorpSecret
                    ? `••••••••  ${t("wecom.secretKept")}`
                    : t("wecom.corpSecretPlaceholder")
                }
                onChange={(e) => setCorpSecretDraft(e.target.value)}
                onBlur={() => {
                  const v = corpSecretDraft.trim();
                  if (!v) {
                    setCorpSecretDraft("");
                    return;
                  }
                  void push({}, undefined, v);
                }}
              />
            }
            hint={<Hint summary={t("wecom.corpHintShort")} details={t("wecom.corpHint")} />}
          />
        </div>
      </div>

      {/* 2 · 回答设置：谁来回答、按什么口径回答、在哪个工作区回答 */}
      <div className="net-panel" hidden={section !== "answer"}>
        <PanelHead title={t("wecom.groupAnswer")} />
        <div className="wecom-rows">
          <div className="wecom-subhead">{t("wecom.subEngine")}</div>

          {/* 引擎与它的实时状态同在一行：选了 DSH 就在下拉右边直接报状态，不再另起一空格子 */}
          <Row
            wide
            label={t("wecom.engineLabel")}
            control={
              <>
                <select
                  className="input-field wecom-split"
                  value={engine}
                  disabled={busy}
                  onChange={(e) => void push({ wecomEngine: e.target.value })}
                >
                  <option value="tianji">{t("wecom.engineTianji")}</option>
                  <option value="dsh">{t("wecom.engineDsh")}</option>
                </select>
                {engine === "dsh" ? (
                  <>
                    <span
                      className={`wecom-status-pill ${dshStateClass(dshState)}`}
                      title={status?.dsh?.detail || undefined}
                    >
                      <i className="wecom-status-dot" aria-hidden />
                      {dshLabel}
                    </span>
                    {status?.dsh?.cliVersion || status?.dsh?.version ? (
                      <span
                        className="wecom-chip"
                        title={[
                          status.dsh.cliVersion
                            ? `DeepSeek Harness v${status.dsh.cliVersion}`
                            : "",
                          status.dsh.version ? `ACP v${status.dsh.version}` : "",
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      >
                        {status.dsh.cliVersion
                          ? `DSH v${status.dsh.cliVersion}`
                          : `ACP v${status.dsh.version}`}
                      </span>
                    ) : null}
                  </>
                ) : null}
              </>
            }
            hint={
              <>
                <Hint summary={t("wecom.engineHintShort")} details={t("wecom.engineHint")} />
                {engine === "dsh" && status?.dsh?.detail ? (
                  <p className="setting-hint">{status.dsh.detail}</p>
                ) : null}
              </>
            }
          />

          {engine === "dsh" ? (
            <Row
              wide
              label={t("wecom.dshMirror")}
              control={
                <>
                  <span
                    className={`wecom-status-pill ${mirror ? "is-connected" : "is-pending"}`}
                    title={mirror?.warning || undefined}
                  >
                    <i className="wecom-status-dot" aria-hidden />
                    {t("wecom.dshMirrorPill", { count: mirror?.bundles ?? 0 })}
                  </span>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => void resyncProfile()}
                  >
                    {t("wecom.dshResync")}
                  </button>
                  {mirror ? (
                    <>
                      <span
                        className="wecom-mirror-path"
                        title={t("wecom.dshMirrorLine", {
                          source: mirror.source,
                          target: mirror.target,
                          time: new Date(mirror.syncedAt).toLocaleString(),
                        })}
                      >
                        {mirror.source}
                        <i aria-hidden>→</i>
                        {mirror.target}
                      </span>
                      <span className="wecom-mirror-time" title={new Date(mirror.syncedAt).toLocaleString()}>
                        {fmtStamp(mirror.syncedAt)}
                      </span>
                    </>
                  ) : (
                    <span className="setting-hint wecom-row-meta">{t("wecom.dshMirrorNone")}</span>
                  )}
                </>
              }
              hint={
                <>
                  <Hint summary={t("wecom.dshMirrorHintShort")} details={t("wecom.dshMirrorHint")} />
                  {mirror?.warning ? <p className="setting-hint">{mirror.warning}</p> : null}
                </>
              }
            />
          ) : null}

          {engine === "dsh" ? (
            <Row
              wide
              label={t("wecom.dshModelRow")}
              control={
                <>
                  <GroupedSelect
                    className="wecom-split"
                    value={modelValue}
                    disabled={busy || catalogBusy}
                    fallbackLabel={t("wecom.dshModelDefault")}
                    staleLabel={t("wecom.dshModelMissing")}
                    searchPlaceholder={t("wecom.dshModelSearch")}
                    emptyLabel={t("wecom.dshModelNoMatch")}
                    groups={(catalog?.groups ?? []).map((group) => ({
                      key: group.group || group.name,
                      label: group.name || group.group,
                      options: group.options.map((option) => ({
                        value: option.value,
                        label: option.name || option.value,
                        hint: option.description,
                      })),
                    }))}
                    onChange={(next) => void push({ wecomDshModel: next })}
                  />
                  <select
                    className="input-field wecom-split-effort"
                    value={settings.wecomDshEffort ?? ""}
                    disabled={busy || catalogBusy || !catalog?.effort}
                    onChange={(e) => void push({ wecomDshEffort: e.target.value })}
                  >
                    <option value="">{t("wecom.dshEffortDefault")}</option>
                    {(catalog?.effort?.options ?? [])
                      .filter((option) => Boolean(option.value))
                      .map((option) => (
                        <option key={option.value} value={option.value}>
                          {option.name || option.value}
                        </option>
                      ))}
                  </select>
                  {/* 模型 / 思考强度 / 刷新 三件套同在控件行，说明只留一句放下面 */}
                  <button
                    type="button"
                    className="btn btn-secondary btn-small wecom-split-refresh"
                    disabled={busy || catalogBusy || !dshReady}
                    onClick={() => void loadModels(true)}
                  >
                    {catalogBusy ? t("wecom.dshLoading") : t("wecom.dshRefreshModels")}
                  </button>
                </>
              }
              hint={
                <p className="setting-hint">
                  {catalogError
                    ? catalogError
                    : !dshReady
                      ? t("wecom.dshModelsOff")
                      : catalog && modelCount === 0
                        ? t("wecom.dshModelsEmpty")
                        : t("wecom.dshModelsHint", { count: modelCount })}
                </p>
              }
            />
          ) : null}
          <div className="wecom-subhead">{t("wecom.subAnswer")}</div>

          <Row
            label={t("wecom.answerMode")}
            control={
              <select
                className="input-field"
                value={settings.wecomAnswerMode ?? "auto"}
                onChange={(e) => void push({ wecomAnswerMode: e.target.value })}
              >
                <option value="auto">{t("wecom.answerMode.auto")}</option>
                <option value="doc">{t("wecom.answerMode.doc")}</option>
                <option value="doc_code">{t("wecom.answerMode.docCode")}</option>
                <option value="code">{t("wecom.answerMode.code")}</option>
              </select>
            }
            hint={<Hint summary={t("wecom.answerModeHintShort")} details={t("wecom.answerModeHint")} />}
          />

          <Row
            wide
            label={t("wecom.selfIntro")}
            control={
              <textarea
                className="input-field"
                rows={6}
                value={selfIntro.value}
                placeholder={t("wecom.selfIntroPlaceholder")}
                onFocus={() => {
                  selfIntro.focused.current = true;
                }}
                onChange={(e) => selfIntro.setValue(Array.from(e.target.value).slice(0, 2000).join(""))}
                onBlur={() => {
                  selfIntro.focused.current = false;
                  const v = selfIntro.value.trim();
                  if (v !== (settings.wecomSelfIntro ?? "").trim()) void push({ wecomSelfIntro: v });
                }}
              />
            }
            hint={
              <p className="setting-hint">
                {t("wecom.selfIntroHint", { count: Array.from(selfIntro.value).length })}
              </p>
            }
          />

          <Row
            label={t("wecom.docDir")}
            control={
              <input
                className="input-field"
                value={docDir.value}
                placeholder="doc"
                onFocus={() => {
                  docDir.focused.current = true;
                }}
                onChange={(e) => docDir.setValue(e.target.value)}
                onBlur={() => {
                  docDir.focused.current = false;
                  const v = docDir.value.trim();
                  if (v !== (settings.wecomDocDir ?? "")) void push({ wecomDocDir: v || undefined });
                }}
              />
            }
          />

          <div className="wecom-subhead">{t("wecom.subWorkspace")}</div>

          <Row
            wide
            label={t("wecom.repoPath")}
            control={
              <input
                className="input-field"
                value={repoPath.value}
                placeholder={settings.currentGitRepo || t("wecom.repoPlaceholder")}
                onFocus={() => {
                  repoPath.focused.current = true;
                }}
                onChange={(e) => repoPath.setValue(e.target.value)}
                onBlur={() => {
                  repoPath.focused.current = false;
                  const v = repoPath.value.trim();
                  if (v !== (settings.wecomRepoPath ?? "")) void push({ wecomRepoPath: v || undefined });
                }}
              />
            }
            hint={engine === "dsh" ? <p className="setting-hint">{t("wecom.dshWorkspaceHint")}</p> : undefined}
          />
        </div>
      </div>

      {/* 3 · 线上排障：查日志与调用链的证据源 */}
      <div className="net-panel" hidden={section !== "trouble"}>
        <PanelHead title={t("wecom.groupTroubleshoot")} />
        <div className="wecom-rows">
          <Row
            label={t("wecom.skywalkingAddr")}
            control={
              <input
                className="input-field"
                value={skywalkingAddr.value}
                placeholder="http://skywalking-oap:12800"
                onFocus={() => {
                  skywalkingAddr.focused.current = true;
                }}
                onChange={(e) => skywalkingAddr.setValue(e.target.value)}
                onBlur={() => {
                  skywalkingAddr.focused.current = false;
                  const v = skywalkingAddr.value.trim();
                  if (v !== (settings.wecomSkywalkingAddr ?? ""))
                    void push({ wecomSkywalkingAddr: v || undefined });
                }}
              />
            }
            hint={<Hint summary={t("wecom.skywalkingHintShort")} details={t("wecom.skywalkingHint")} />}
          />

          <Row
            label={t("wecom.skywalkingTimezone")}
            control={
              <select
                className="input-field"
                aria-label={t("wecom.skywalkingTimezone")}
                value={settings.wecomSkywalkingTimezoneOffsetMinutes ?? ""}
                disabled={busy}
                onChange={(e) =>
                  void push({
                    wecomSkywalkingTimezoneOffsetMinutes:
                      e.target.value === "" ? undefined : Number(e.target.value),
                  })
                }
              >
                <option value="">{t("wecom.skywalkingTimezoneLocal")}</option>
                <option value={0}>UTC</option>
                <option value={480}>UTC+8</option>
                <option value={540}>UTC+9</option>
                <option value={330}>UTC+5:30</option>
                <option value={-300}>UTC-5</option>
                <option value={-480}>UTC-8</option>
              </select>
            }
            hint={<p className="setting-hint">{t("wecom.skywalkingTimezoneHint")}</p>}
          />

          <Row
            wide
            label={t("wecom.skywalkingHeader")}
            control={
              <input
                className="input-field"
                value={skywalkingHeader.value}
                placeholder="Authorization: Bearer xxx"
                onFocus={() => {
                  skywalkingHeader.focused.current = true;
                }}
                onChange={(e) => skywalkingHeader.setValue(e.target.value)}
                onBlur={() => {
                  skywalkingHeader.focused.current = false;
                  const v = skywalkingHeader.value.trim();
                  if (v !== (settings.wecomSkywalkingHeader ?? ""))
                    void push({ wecomSkywalkingHeader: v || undefined });
                }}
              />
            }
          />
        </div>
      </div>

      {/* 4 · 逆向文档自动更新：到点拉分支、改文档、提交、推群 */}
      <div className="net-panel" hidden={section !== "autoupdate"}>
        <PanelHead title={t("wecom.groupAutoupdate")} />
        <div className="wecom-rows">
          <Row
            asSwitch
            label={t("wecom.autoUpdateEnabled")}
            control={
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={Boolean(settings.wecomAutoUpdateEnabled)}
                  onChange={(e) => void push({ wecomAutoUpdateEnabled: e.target.checked })}
                />
                <span className="toggle-slider" />
              </label>
            }
            hint={
              <>
                <Hint summary={t("wecom.autoUpdateHintShort")} details={t("wecom.autoUpdateHint")} />
                <p className="setting-hint">{t("wecom.docgenChecklist")}</p>
              </>
            }
          />

          <Row
            label={t("wecom.autoUpdateTime")}
            control={
              <input
                className="input-field"
                type="time"
                value={autoUpdateTime.value || "23:00"}
                onFocus={() => {
                  autoUpdateTime.focused.current = true;
                }}
                onChange={(e) => autoUpdateTime.setValue(e.target.value)}
                onBlur={() => {
                  autoUpdateTime.focused.current = false;
                  const v = autoUpdateTime.value.trim();
                  if (v !== (settings.wecomAutoUpdateTime ?? ""))
                    void push({ wecomAutoUpdateTime: v || undefined });
                }}
              />
            }
          />

          <Row
            label={t("wecom.releaseBranch")}
            control={
              <input
                className="input-field"
                value={releaseBranch.value}
                placeholder="release"
                onFocus={() => {
                  releaseBranch.focused.current = true;
                }}
                onChange={(e) => releaseBranch.setValue(e.target.value)}
                onBlur={() => {
                  releaseBranch.focused.current = false;
                  const v = releaseBranch.value.trim();
                  if (v !== (settings.wecomReleaseBranch ?? ""))
                    void push({ wecomReleaseBranch: v || undefined });
                }}
              />
            }
          />

          <Row
            wide
            label={t("wecom.webhookUrl")}
            control={
              <input
                className="input-field"
                type="password"
                value={webhookDraft}
                placeholder={
                  settings.wecomWebhookUrl
                    ? `••••••••  ${t("wecom.secretKept")}`
                    : t("wecom.webhookPlaceholder")
                }
                onChange={(e) => setWebhookDraft(e.target.value)}
                onBlur={() => {
                  const v = webhookDraft.trim();
                  if (!v) {
                    setWebhookDraft("");
                    return;
                  }
                  void push({ wecomWebhookUrl: v }).then(() => setWebhookDraft(""));
                }}
              />
            }
            hint={<Hint summary={t("wecom.webhookHintShort")} details={t("wecom.webhookHint")} />}
          />

          <Row
            asSwitch
            label={t("wecom.autoCommit")}
            control={
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={settings.wecomAutoCommit !== false}
                  onChange={(e) => void push({ wecomAutoCommit: e.target.checked })}
                />
                <span className="toggle-slider" />
              </label>
            }
            hint={<Hint summary={t("wecom.autoCommitHintShort")} details={t("wecom.autoCommitHint")} />}
          />

          <Row
            asSwitch
            label={t("wecom.autoPush")}
            control={
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={Boolean(settings.wecomAutoPush)}
                  disabled={settings.wecomAutoCommit === false}
                  onChange={(e) => void push({ wecomAutoPush: e.target.checked })}
                />
                <span className="toggle-slider" />
              </label>
            }
            hint={<p className="setting-hint">{t("wecom.autoPushHint")}</p>}
          />

          <Row
            wide
            label={t("wecom.docgenRunNow")}
            control={
              <div className="wecom-inline">
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={docgenBusy}
                  onClick={() => {
                    setDocgenBusy(true);
                    setMsg("");
                    void testWecomDocgen()
                      .then((r) => setMsg(r))
                      .catch((e) => setMsg(String(e)))
                      .finally(() => setDocgenBusy(false));
                  }}
                >
                  {t("wecom.docgenTestBtn")}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={docgenBusy}
                  onClick={() => {
                    setDocgenBusy(true);
                    setMsg("");
                    void runWecomDocgenNow()
                      .then((r) => setMsg(r))
                      .catch((e) => setMsg(String(e)))
                      .finally(() => setDocgenBusy(false));
                  }}
                >
                  {docgenBusy ? t("wecom.docgenRunning") : t("wecom.docgenRunNowBtn")}
                </button>
              </div>
            }
            hint={<p className="setting-hint">{t("wecom.docgenRunNowHint")}</p>}
          />
        </div>
      </div>

      {/* 5 · 访问控制：谁能用 + 能用什么 + 配对授权 */}
      <div className="net-panel" hidden={section !== "access"}>
        <PanelHead title={t("wecom.groupAccess")} />
        <div className="wecom-rows">
          <div className="wecom-subhead">{t("wecom.subWho")}</div>

          <Row
            label={t("wecom.dmPolicy")}
            control={
              <select
                className="input-field"
                value={settings.wecomDmPolicy ?? "pairing"}
                onChange={(e) => void push({ wecomDmPolicy: e.target.value })}
              >
                <option value="pairing">{t("wecom.policy.pairing")}</option>
                <option value="allowlist">{t("wecom.policy.allowlist")}</option>
                <option value="open">{t("wecom.policy.open")}</option>
                <option value="disabled">{t("wecom.policy.disabled")}</option>
              </select>
            }
          />

          <Row
            label={t("wecom.groupPolicy")}
            control={
              <select
                className="input-field"
                value={settings.wecomGroupPolicy ?? "allowlist"}
                onChange={(e) => void push({ wecomGroupPolicy: e.target.value })}
              >
                <option value="allowlist">{t("wecom.policy.allowlist")}</option>
                <option value="open">{t("wecom.policy.open")}</option>
                <option value="disabled">{t("wecom.policy.disabled")}</option>
              </select>
            }
          />

          <Row
            label={t("wecom.allowUsers")}
            control={
              <input
                className="input-field"
                value={allowUsers.value}
                placeholder="userid1, userid2"
                onFocus={() => {
                  allowUsers.focused.current = true;
                }}
                onChange={(e) => allowUsers.setValue(e.target.value)}
                onBlur={() => {
                  allowUsers.focused.current = false;
                  const next = parseList(allowUsers.value);
                  const prev = settings.wecomAllowUsers ?? [];
                  if (next.join(",") !== prev.join(",")) void push({ wecomAllowUsers: next });
                }}
              />
            }
          />

          <Row
            label={t("wecom.allowGroups")}
            control={
              <input
                className="input-field"
                value={allowGroups.value}
                placeholder="chatid1, chatid2"
                onFocus={() => {
                  allowGroups.focused.current = true;
                }}
                onChange={(e) => allowGroups.setValue(e.target.value)}
                onBlur={() => {
                  allowGroups.focused.current = false;
                  const next = parseList(allowGroups.value);
                  const prev = settings.wecomAllowGroups ?? [];
                  if (next.join(",") !== prev.join(",")) void push({ wecomAllowGroups: next });
                }}
              />
            }
          />

          <Row
            asSwitch
            label={t("wecom.requireMention")}
            control={
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={settings.wecomRequireMention !== false}
                  onChange={(e) => void push({ wecomRequireMention: e.target.checked })}
                />
                <span className="toggle-slider" />
              </label>
            }
            hint={<Hint summary={t("wecom.requireMentionHintShort")} details={t("wecom.requireMentionHint")} />}
          />

          <div className="wecom-subhead">{t("wecom.subWhat")}</div>

          <Row
            label={t("wecom.permission")}
            control={
              <select
                className="input-field"
                value={settings.wecomPermission ?? "read"}
                onChange={(e) => {
                  const v = e.target.value;
                  if (v === "full") {
                    void (async () => {
                      const ok = await confirm({
                        title: t("wecom.permission"),
                        message: t("wecom.fullConfirm"),
                      });
                      if (!ok) return;
                      await push({ wecomPermission: v });
                    })();
                    return;
                  }
                  void push({ wecomPermission: v });
                }}
              >
                <option value="read">{t("wecom.perm.read")}</option>
                <option value="workspace">{t("wecom.perm.workspace")}</option>
                <option value="review">{t("wecom.perm.review")}</option>
                <option value="full">{t("wecom.perm.full")}</option>
              </select>
            }
            hint={
              engine === "dsh" ? (
                <Hint summary={t("wecom.dshPermHintShort")} details={t("wecom.dshPermNote")} />
              ) : undefined
            }
          />

          <Row
            label={t("wecom.maxJobs")}
            control={
              <select
                className="input-field"
                aria-label={t("wecom.maxJobs")}
                value={maxJobs}
                disabled={busy}
                onChange={(e) => void push({ wecomMaxJobs: Number(e.target.value) })}
              >
                {maxJobsChoices.map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
              </select>
            }
            hint={<p className="setting-hint">{t("wecom.maxJobsHint")}</p>}
          />

          <div className="wecom-subhead">{t("wecom.sessionRules")}</div>
          <div className="wecom-row is-wide">
            <Hint summary={t("wecom.sessionRulesShort")} details={t("wecom.sessionRulesHint")} />
            <Hint summary={t("wecom.sharedTasksShort")} details={t("wecom.sharedTasksHint")} />
            <p className="setting-hint">{t("wecom.sessionCommandsHint")}</p>
            <p className="setting-hint">{t("wecom.groupVisibilityHint")}</p>
            <p className="setting-hint">{t("wecom.sessionMigrationHint")}</p>
            <p className="setting-hint">{t("wecom.connectChecklistHint")}</p>
          </div>

          <div className="wecom-subhead">
            <span>{t("wecom.groupPairing")}</span>
            {(settings.wecomPairedUsers ?? []).length > 0 ? (
              <span className="wecom-count-badge">{(settings.wecomPairedUsers ?? []).length}</span>
            ) : pairings.length > 0 ? (
              <span className="wecom-count-badge is-pending">{pairings.length}</span>
            ) : null}
          </div>

        <div className="setting-item">
          <span className="setting-label">{t("wecom.pairingTitle")}</span>
          {pairings.length === 0 ? (
            <p className="setting-hint">{t("wecom.pairingEmpty")}</p>
          ) : (
            <>
              <p className="setting-hint">{t("wecom.pairingRestartHint")}</p>
              <ul className="wecom-pairing-list">
              {pairings.map((p) => (
                <li key={p.code} className="wecom-pairing-row">
                  <code className="tabular-nums wecom-pair-code">{p.code}</code>
                  <span className="wecom-pair-user">{p.userid}</span>
                  <button
                    type="button"
                    className="btn btn-primary btn-small"
                    disabled={busy}
                    onClick={() => {
                      setBusy(true);
                      void approveWecomPairing(p.code)
                        .then(async (uid) => {
                          const paired = [...(settings.wecomPairedUsers ?? [])];
                          if (!paired.includes(uid)) paired.push(uid);
                          setSettings({ wecomPairedUsers: paired });
                          setMsg(t("wecom.paired", { userid: uid }));
                          await refresh();
                        })
                        .catch((e) => setMsg(String(e)))
                        .finally(() => setBusy(false));
                    }}
                  >
                    {t("wecom.approve")}
                  </button>
                </li>
              ))}
            </ul>
            </>
          )}
          {(settings.wecomPairedUsers ?? []).length > 0 && (
            <div className="wecom-paired">
              <p className="setting-hint">{t("wecom.pairedList")}</p>
              <div className="net-grid">
                {(settings.wecomPairedUsers ?? []).map((uid) => (
                  <div key={uid} className="wecom-pairing-row">
                    <span className="wecom-pair-user">{uid}</span>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      disabled={busy}
                      onClick={() => {
                        setBusy(true);
                        void revokeWecomUser(uid)
                          .then(() => {
                            const paired = (settings.wecomPairedUsers ?? []).filter((u) => u !== uid);
                            void push({ wecomPairedUsers: paired });
                          })
                          .catch((e) => setMsg(String(e)))
                          .finally(() => setBusy(false));
                      }}
                    >
                      {t("wecom.revoke")}
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        </div>
      </div>

      {/* 6 · 运行诊断：连不上 / 答得不对时再看，两块日志各自滚动 */}
      <div className="net-panel wecom-diag-panel" hidden={section !== "diag"}>
        <PanelHead
          title={t("wecom.groupDiag")}
          aside={<span className="wecom-diag-hint">{t("wecom.groupDiagHint")}</span>}
        />
        <WecomRequestDiagnostics selection={diagnosticSelection} />
        <section className="wecom-diag-block">
          <div className="wecom-diag-head">
            <span>{t("wecom.diag.performance")}</span>
          </div>
          <p className="setting-hint">{t("wecom.diag.performanceHint")}</p>
          {performance ? (
            <>
              <p className="setting-hint">
                {t("wecom.diag.performanceRequestId")} <code>{performance.requestId}</code>
              </p>
              <div className="wecom-inline">
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
                <details className="wecom-hint" key={performanceSummary.requestId}>
                  <summary>{t("wecom.diag.phaseTable")}</summary>
                  <p className="setting-hint">
                    {t("wecom.diag.performanceRequestId")} <code>{performanceSummary.requestId}</code>
                  </p>
                  <p className="setting-hint">{t("wecom.diag.phaseTableHint")}</p>
                  <div className="wecom-inline">
                    <span className="wecom-chip">{t("wecom.diag.totalElapsed", { duration: reportedMs(performanceSummary.totalMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.queueElapsed", { duration: reportedMs(performanceSummary.queueMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.elapsedWithQueue", { duration: reportedMs(performanceSummary.elapsedWithQueueMs) })}</span>
                    <span className="wecom-chip">{t("wecom.diag.sessionAction", {
                      action: performanceSummary.sessionAction
                        ? t(`wecom.diag.sessionActions.${performanceSummary.sessionAction}`, { defaultValue: performanceSummary.sessionAction })
                        : t("wecom.diag.notReported"),
                    })}</span>
                  </div>
                  <p className="setting-hint">{t("wecom.diag.modelReported", { model: performanceSummary.model || t("wecom.diag.notReported"), effort: performanceSummary.effort || t("wecom.diag.notReported") })}</p>
                  {Array.isArray(performanceSummary.phases) && performanceSummary.phases.length > 0 ? (
                    <div className="wecom-diag-log" tabIndex={0} role="region" aria-label={t("wecom.diag.phaseTable")}>
                      <table>
                        <thead><tr>
                          {(["phase", "elapsed", "calls", "input", "output", "tools", "first"] as const).map((column) => (
                            <th scope="col" key={column}>{t(`wecom.diag.columns.${column}`)}</th>
                          ))}
                        </tr></thead>
                        <tbody>{performanceSummary.phases.map((row, index) => (
                          <tr key={`${row.phase}-${index}`}>
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
                  ) : <p className="setting-hint">{t("wecom.diag.notReported")}</p>}
                  <p className="setting-hint">{t("wecom.diag.validationFailures", {
                    countText: typeof performanceSummary.validationFailureCount === "number"
                      && Number.isFinite(performanceSummary.validationFailureCount) && performanceSummary.validationFailureCount >= 0
                      ? reportedCount(performanceSummary.validationFailureCount)
                      : t("wecom.diag.validationNotReported"),
                  })}</p>
                  {performanceSummary.validationFailures?.map((failure, index) => (
                    <p className="setting-hint" key={`${index}-${failure}`}>{failure}</p>
                  ))}
                </details>
              ) : null}
            </>
          ) : <p className="setting-hint">{t("wecom.diag.notReported")}</p>}
          <p className="setting-hint">
            {t("wecom.diag.cacheHits", { countText: reportedCount(investigation?.cacheHits) })}
            {investigation?.requestId ? <> · {t("wecom.diag.requestId")} <code>{investigation.requestId}</code></> : null}
          </p>
          <p className="setting-hint">{t("wecom.diag.cacheHitsHint")}</p>
        </section>
        <div className="wecom-diag-grid">
          <section className="wecom-diag-block">
            <div className="wecom-diag-head">
              <span>{t("wecom.diag.channelLog")}</span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={logBusy}
                onClick={() => void loadLog()}
              >
                {t("wecom.diag.refresh")}
              </button>
            </div>
            <p className="setting-hint">{t("wecom.diag.logHint")}</p>
            <pre className="wecom-diag-log">{logText || (logBusy ? "…" : t("wecom.diag.empty"))}</pre>
          </section>
          <section className="wecom-diag-block">
            <div className="wecom-diag-head">
              <span>{t("wecom.diag.sessionLog")}</span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={sessionBusy}
                onClick={() => void loadSessionLog()}
              >
                {t("wecom.diag.refresh")}
              </button>
            </div>
            <p className="setting-hint">{t("wecom.diag.sessionHint")}</p>
            {sessionError ? <p className="setting-hint" role="alert">{t("wecom.diag.sessionReadFailed")}</p> : null}
            {sessionPage ? <>
              <p className="setting-hint">{t("wecom.diag.sessionCoverage", { count: sessionPage.entries.length, bytes: sessionPage.scannedBytes })}</p>
              {sessionPage.partial ? <p className="setting-hint" role="status">{t("wecom.diag.sessionPartial")}</p> : null}
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
                      <button type="button" className="btn btn-secondary btn-small"
                        onClick={() => setDiagnosticSelection((previous) => ({ requestId: entry.requestId!, revision: (previous?.revision ?? 0) + 1 }))}>
                        {t("wecom.diag.requests.select")} <code>{entry.requestId}</code>
                      </button>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      </div>

      {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
    </div>
  );
}
