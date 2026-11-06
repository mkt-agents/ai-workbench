import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Eye,
  EyeOff,
  Bot,
  QrCode,
  ShieldCheck,
  FolderGit2,
  FolderOpen,
  MessageSquare,
  Users,
  ListChecks,
  UserCheck,
  Cpu,
  Sparkles,
  Gauge,
  Terminal,
  MessagesSquare,
  Activity,
} from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import GroupedSelect from "./GroupedSelect";
import WecomRequestDiagnostics from "./WecomRequestDiagnostics";
import { useGlobalStore } from "../core/store";
import type { AppSettings, DshModelCatalog, WecomSkywalkingEnv } from "../core/types";
import {
  applyWecomChannel,
  buildWecomSkywalkingPatch,
  protectSecret,
  revealWecomSecret,
  approveWecomPairing,
  fetchDshModels,
  fetchWecomLog,
  fetchWecomPairings,
  fetchWecomSessionLogPage,
  fetchWecomStatus,
  normalizeWecomMaxJobs,
  resolveWecomSkywalking,
  restartWecomChannel,
  revokeWecomUser,
  startWecomQr,
  pollWecomQr,
  syncDshProfile,
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
type WecomSection = "channel" | "answer" | "access" | "diag";

/**
 * 扁平分区标题：与参考图一致，不使用图标卡片；右侧可挂开关 / 计数。
 *
 * `bare` 用于「上面的 tab 已经写明这页叫什么」的场景：不再重复画标题，
 * 把右挂件（二级页签）直接放到左边当这一页的主控件。
 */
function PanelHead({
  title,
  aside,
  bare = false,
}: {
  title?: string;
  aside?: React.ReactNode;
  bare?: boolean;
}) {
  return (
    <div className={`wecom-panel-head${bare ? " is-bare" : ""}`}>
      {bare ? null : <div className="hub-group-title">{title}</div>}
      {aside ? <div className="wecom-panel-aside">{aside}</div> : null}
    </div>
  );
}

export default function WecomSettingsCard() {
  const { t } = useTranslation("settings");
  const confirm = useConfirm();
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);
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
  const [qrBusy, setQrBusy] = useState(false);
  const [qrScode, setQrScode] = useState("");
  const [qrAuthUrl, setQrAuthUrl] = useState("");
  const [qrSvg, setQrSvg] = useState("");
  const [qrPageUrl, setQrPageUrl] = useState("");
  const [qrDetail, setQrDetail] = useState("");
  const qrPollRef = useRef<number | null>(null);
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
      if (qrPollRef.current != null) {
        window.clearInterval(qrPollRef.current);
        qrPollRef.current = null;
      }
    };
  }, []);

  const skywalking = resolveWecomSkywalking(settings);
  const skywalkingEnv = skywalking.env;
  const skywalkingProfile = skywalking.profile;

  // 旧扁平配置一次性迁入 profiles.prod，避免反复推断
  useEffect(() => {
    const r = resolveWecomSkywalking(useGlobalStore.getState().settings);
    if (r.needsMigrate) setSettings(r.migrated);
  }, [setSettings]);

  const botId = useFieldDraft(settings.wecomBotId ?? "");
  const corpId = useFieldDraft(settings.wecomCorpId ?? "");
  const displayName = useFieldDraft(settings.wecomBotDisplayName ?? "");
  const allowUsers = useFieldDraft((settings.wecomAllowUsers ?? []).join(", "));
  const allowGroups = useFieldDraft((settings.wecomAllowGroups ?? []).join(", "));
  const repoPath = useFieldDraft(settings.wecomRepoPath ?? "");
  const docDir = useFieldDraft(settings.wecomDocDir ?? "");
  const selfIntro = useFieldDraft(settings.wecomSelfIntro ?? "");
  const enhanceDm = useFieldDraft(settings.wecomEnhanceDm ?? "");
  const enhanceGroup = useFieldDraft(settings.wecomEnhanceGroup ?? "");
  const [enhanceTab, setEnhanceTab] = useState<"dm" | "group">("dm");
  /** 回答设置页的二级页签：引擎与工作区 / 回答与上下文 / 线上排障 */
  const [answerView, setAnswerView] = useState<"engine" | "answer" | "trouble">("engine");
  const skywalkingAddr = useFieldDraft(skywalkingProfile.addr ?? "");
  const skywalkingHeader = useFieldDraft(skywalkingProfile.header ?? "");

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

  /** 原生目录选择框。工作区直接用绝对路径；文档目录约定相对工作区，
   *  选中的目录若在工作区之下就折算成相对路径（正斜杠），否则原样存。 */
  const pickAnswerDir = async (key: "wecomRepoPath" | "wecomDocDir") => {
    const dir = await invokePickDirectory();
    if (!dir) return;
    if (key === "wecomRepoPath") {
      repoPath.focused.current = false;
      repoPath.setValue(dir);
      void push({ wecomRepoPath: dir });
      return;
    }
    const rawWs = (
      repoPath.value.trim() ||
      settings.wecomRepoPath ||
      settings.currentGitRepo ||
      ""
    ).trim();
    const wsSeg = rawWs.replace(/\//g, "\\").replace(/\\+$/, "");
    let value = dir;
    if (wsSeg && dir.toLowerCase().startsWith(wsSeg.toLowerCase())) {
      value = dir
        .slice(wsSeg.length)
        .replace(/\\/g, "/")
        .replace(/^\/+/, "");
    }
    if (!value) value = "doc";
    docDir.focused.current = false;
    docDir.setValue(value);
    void push({ wecomDocDir: value || undefined });
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

  const loadModels = useCallback(async (refresh: boolean, scope?: "channel" | "docgen") => {
    setCatalogBusy(true);
    setCatalogError("");
    try {
      setCatalog(await fetchDshModels(refresh, scope));
    } catch (e) {
      setCatalogError(String(e));
    } finally {
      setCatalogBusy(false);
    }
  }, []);

  // 引擎就绪后再拉模型目录；DSH 没起来时不主动去启进程
  // （进页面就去 spawn 一个 node 只会让人以为界面卡住）。
  useEffect(() => {
    if (catalog) return;
    if (engine === "dsh" && dshReady) void loadModels(false, "channel");
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

  // 后端把「本轮没下发 reasoning_effort」记成 default（走服务商默认档），
  // 和真正解析不出值的情况区分开，面板上别都写成「未知」。
  const reportedEffort = (value: string | null | undefined) =>
    !value || value === "default" ? t("wecom.diag.effortDefault") : value;

  const maxJobs = normalizeWecomMaxJobs(settings.wecomMaxJobs);
  // 保留已保存的 3/6 或其它合法值，避免原生 select 无匹配项而显示错误档位。
  const maxJobsChoices = MAX_JOBS_CHOICES.includes(maxJobs)
    ? MAX_JOBS_CHOICES
    : [...MAX_JOBS_CHOICES, maxJobs].sort((a, b) => a - b);
  const modelValue = settings.wecomDshModel ?? "";
  const modelCount =
    catalog?.groups.reduce((total, group) => total + group.options.length, 0) ?? 0;
  // 增强提示词按当前选中侧（私聊 / 群聊）取开关与正文，卡片头和底部计数都用这一份
  const enhanceEnabled =
    enhanceTab === "dm"
      ? Boolean(settings.wecomEnhanceDmEnabled)
      : Boolean(settings.wecomEnhanceGroupEnabled);
  const enhanceValue = enhanceTab === "dm" ? enhanceDm.value : enhanceGroup.value;
  const enhanceCount = Array.from(enhanceValue).length;

  const stopQrPoll = () => {
    if (qrPollRef.current != null) {
      window.clearInterval(qrPollRef.current);
      qrPollRef.current = null;
    }
  };

  const startQrBind = async () => {
    setQrBusy(true);
    setMsg("");
    setQrDetail("");
    stopQrPoll();
    try {
      const started = await startWecomQr();
      if (!mounted.current) return;
      setQrScode(started.scode);
      setQrAuthUrl(started.authUrl);
      setQrSvg(started.qrSvg || "");
      setQrPageUrl(started.qrPageUrl);
      setQrDetail(t("wecom.qrWaiting"));
      const startedAt = Date.now();
      qrPollRef.current = window.setInterval(() => {
        void (async () => {
          if (Date.now() - startedAt > 5 * 60 * 1000) {
            stopQrPoll();
            if (mounted.current) {
              setQrDetail(t("wecom.qrExpired"));
              setQrBusy(false);
            }
            return;
          }
          try {
            const polled = await pollWecomQr(started.scode);
            if (!mounted.current) return;
            setQrDetail(polled.detail || t("wecom.qrWaiting"));
            if (polled.status === "expired") {
              stopQrPoll();
              setQrBusy(false);
              return;
            }
            if (polled.status === "success" && polled.botId && polled.secret) {
              stopQrPoll();
              setQrBusy(false);
              setQrScode("");
              setQrAuthUrl("");
              setQrSvg("");
              setBotIdValue(polled.botId);
              await push({ wecomBotId: polled.botId, wecomEnabled: true }, polled.secret);
              setMsg(t("wecom.qrSuccess"));
            }
          } catch (e) {
            if (mounted.current) setQrDetail(String(e));
          }
        })();
      }, 3000);
    } catch (e) {
      setMsg(String(e));
      setQrBusy(false);
    }
  };

  const setBotIdValue = (v: string) => {
    botId.setValue(v);
  };

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
          <div className="wecom-fact">
            <span className="wecom-fact-label">{t("wecom.diag.factCrossQuery")}</span>
            <span className="wecom-fact-items">
              {status?.lastCrossQuery ? (
                <>
                  <span className="wecom-chip">
                    {t("wecom.diag.crossDocs", { countText: reportedCount(status.lastCrossQuery.docHits) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.crossCode", { countText: reportedCount(status.lastCrossQuery.codeHits) })}
                  </span>
                  <span className="wecom-chip">
                    {t("wecom.diag.crossLogs", { countText: reportedCount(status.lastCrossQuery.logHits) })}
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
        {/*
          顺序 = 「接入 → 行为 → 准入 → 诊断」，且一个一级 Tab 只装一种性质的内容：
          通道 = 连接（凭据 / 扫码 / 业务 API）；回答设置 = AI 行为（引擎、模型、工作区、
          权限档位、回答模式、上下文增强、SkyWalking 证据源）；访问策略 = 准入（会话准入）；
          运行诊断 = 排障（诊断查询 + 日志）。
          2026-11-06 按性质归堆：并发上限、权限档位与「会话与共享任务」说明从访问策略迁入回答设置，
          连通检查清单拆去运行诊断页顶——访问策略只剩会话准入，通道不再混 AI 行为。
          更早的两次拆分：原「线上排障」并入「回答设置」第三个二级页签（本质是回答问题
          所用的证据源）；原「文档自动更新与推送」独立成侧栏页「逆向工程」
          （工程管理 → 逆向工程，`src/components/ReverseDocPage.tsx`），改文档配置不再重启长连接。
          摘要行 = 每个 Tab 的关键状态一眼可读：通道看连接状态、回答设置看当前引擎、
          访问策略看配对/待审批（有待审时 `is-pending` 高亮）、运行诊断提示日志入口。
          面板本身仍按原来的顺序挂在下面——隐藏面板用的是 `hidden`，不进焦点与无障碍树，
          所以调整顺序只动这张表即可。
        */}
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
              id: "access",
              title: t("wecom.groupAccess"),
              summary:
                pairings.length > 0
                  ? t("wecom.sectionPairedPending", {
                      count: (settings.wecomPairedUsers ?? []).length,
                      pending: pairings.length,
                    })
                  : t("wecom.sectionPaired", { count: (settings.wecomPairedUsers ?? []).length }),
              attention: pairings.length > 0,
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
            <span
              className={`wecom-section-summary${"attention" in item && item.attention ? " is-pending" : ""}`}
            >
              {item.summary}
            </span>
          </button>
        ))}
      </div>

      {/* 1 · 通道与凭据：开了没、连上没、用哪个机器人 */}
      <div className="net-panel" hidden={section !== "channel"}>
        <PanelHead title={t("wecom.groupChannel")} />
        <div className="wecom-rows">
          <div className="wecom-block-card">
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <Bot size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subBot")}</span>
              </div>
              <span
                className={`wecom-status-pill ${statusClass(status?.state)}`}
                title={status?.detail || undefined}
              >
                <i className="wecom-status-dot" aria-hidden />
                {stateLabel}
              </span>
            </div>

            <Row
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

            <div className="wecom-pair">
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
            </div>

            {/* 扫码绑定：左图文案 + 右按钮的操作条，二维码单独展开成舞台 */}
            <div className="wecom-bind">
              <div className="wecom-bind-main">
                <span className="wecom-bind-icon" aria-hidden>
                  <QrCode size={16} />
                </span>
                <div className="wecom-bind-text">
                  <span className="wecom-bind-title">{t("wecom.qrBind")}</span>
                  <p className="setting-hint">{t("wecom.qrBindHint")}</p>
                </div>
              </div>
              <div className="wecom-bind-actions">
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  disabled={busy || qrBusy}
                  onClick={() => void startQrBind()}
                >
                  {qrBusy ? t("wecom.qrWaiting") : t("wecom.qrBind")}
                </button>
                {qrPageUrl ? (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => {
                      void import("@tauri-apps/api/core").then(({ invoke }) =>
                        invoke("open_in_browser", { url: qrPageUrl }).catch(() =>
                          window.open(qrPageUrl, "_blank"),
                        ),
                      );
                    }}
                  >
                    {t("wecom.qrOpenPage")}
                  </button>
                ) : null}
                {qrBusy ? (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => {
                      stopQrPoll();
                      setQrBusy(false);
                      setQrScode("");
                      setQrAuthUrl("");
                      setQrSvg("");
                      setQrPageUrl("");
                      setQrDetail("");
                    }}
                  >
                    {t("wecom.qrCancel")}
                  </button>
                ) : null}
              </div>
            </div>

            {qrAuthUrl ? (
              <div className="wecom-qr-stage">
                <div className="wecom-qr-frame">
                  {qrSvg ? (
                    <div
                      className="wecom-qr-img wecom-qr-svg"
                      role="img"
                      aria-label="WeCom QR"
                      dangerouslySetInnerHTML={{ __html: qrSvg }}
                    />
                  ) : (
                    <div className="wecom-qr-skeleton" aria-hidden>
                      <QrCode size={28} />
                    </div>
                  )}
                </div>
                <div className="wecom-qr-side">
                  <p className="wecom-qr-title">{t("wecom.qrScanning")}</p>
                  {!qrSvg ? <p className="setting-hint">{t("wecom.qrSvgMissing")}</p> : null}
                  {qrDetail ? <p className="setting-hint">{qrDetail}</p> : null}
                  {qrScode ? (
                    <p className="setting-hint">
                      <code className="tabular-nums">{qrScode.slice(0, 8)}…</code>
                    </p>
                  ) : null}
                </div>
              </div>
            ) : null}
          </div>

          <div className="wecom-block-card is-split">
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <ShieldCheck size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subCorp")}</span>
              </div>
            </div>

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
      </div>

      {/* 2 · 回答设置：谁来回答、按什么口径回答、在哪个工作区回答 */}
      <div className="net-panel" hidden={section !== "answer"}>
        {/* 五张卡分三页：「谁来答 / 在哪答」（引擎、模型、工作区）、
            「怎么答 / 补什么料」（问答模式、自我介绍、上下文增强）、
            「拿什么当证据」（线上排障：SkyWalking OAP，回答时补查的日志与调用链源）。 */}
        <PanelHead
          bare
          aside={
            <div
              className="devtools-segmented wecom-head-seg"
              role="group"
              aria-label={t("wecom.groupAnswer")}
            >
              {(["engine", "answer", "trouble"] as const).map((view) => (
                <button
                  key={view}
                  type="button"
                  className={`segmented-item${answerView === view ? " active" : ""}`}
                  aria-pressed={answerView === view}
                  onClick={() => setAnswerView(view)}
                >
                  {t(`wecom.answerView.${view}`)}
                </button>
              ))}
            </div>
          }
        />
        <div className="wecom-rows">
          <div className="wecom-block-card" hidden={answerView !== "engine"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <Cpu size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subEngine")}</span>
              </div>
            </div>

            {/* 引擎选择与配置镜像并排一行：两个字段各占一列（天玑没有镜像行时自动铺满整行），
                引擎的实时状态仍贴在下拉右边 */}
            <div className="wecom-pair">
              <Row
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
                          <span
                            className="wecom-mirror-time"
                            title={new Date(mirror.syncedAt).toLocaleString()}
                          >
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
            </div>

            {engine === "dsh" ? (
              <Row
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
          </div>

          <div className="wecom-block-card" hidden={answerView !== "answer"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <MessageSquare size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subAnswer")}</span>
              </div>
            </div>

            <Row
              label={t("wecom.answerMode")}
              control={
                <select
                  className="input-field wecom-split"
                  value={settings.wecomAnswerMode ?? "auto"}
                  onChange={(e) => void push({ wecomAnswerMode: e.target.value })}
                >
                  <option value="auto">{t("wecom.answerMode.auto")}</option>
                  <option value="doc">{t("wecom.answerMode.doc")}</option>
                  <option value="doc_code">{t("wecom.answerMode.docCode")}</option>
                  <option value="code">{t("wecom.answerMode.code")}</option>
                  <option value="linked">{t("wecom.answerMode.linked")}</option>
                </select>
              }
              hint={<Hint summary={t("wecom.answerModeHintShort")} details={t("wecom.answerModeHint")} />}
            />

            <Row
              label={t("wecom.selfIntro")}
              control={
                <div className="wecom-editor-stack">
                  <textarea
                    className="input-field"
                    rows={8}
                    value={selfIntro.value}
                    placeholder={t("wecom.selfIntroPlaceholder")}
                    onFocus={() => {
                      selfIntro.focused.current = true;
                    }}
                    onChange={(e) =>
                      selfIntro.setValue(Array.from(e.target.value).slice(0, 2000).join(""))
                    }
                    onBlur={() => {
                      selfIntro.focused.current = false;
                      const v = selfIntro.value.trim();
                      if (v !== (settings.wecomSelfIntro ?? "").trim()) {
                        void push({ wecomSelfIntro: v });
                      }
                    }}
                  />
                  <div className="wecom-editor-foot">
                    <span className="wecom-char-count">
                      {t("wecom.charCount", { count: Array.from(selfIntro.value).length })}
                    </span>
                  </div>
                </div>
              }
              hint={
                <Hint summary={t("wecom.selfIntroHintShort")} details={t("wecom.selfIntroHint")} />
              }
            />
          </div>

          {/* 上下文增强：独立小卡片——标题 / 会话类型 / 总开关在头，正文、字数与动作在底部 */}
          <div
            className={`wecom-enhance-card${enhanceEnabled ? " is-on" : ""}`}
            hidden={answerView !== "answer"}
          >
            <div className="wecom-enhance-head">
              <div className="wecom-enhance-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <Sparkles size={14} />
                </span>
                <span className="wecom-enhance-title">{t("wecom.enhanceTitle")}</span>
                <div
                  className="devtools-segmented wecom-enhance-seg"
                  role="group"
                  aria-label={t("wecom.enhanceScope")}
                >
                  {(["dm", "group"] as const).map((tab) => (
                    <button
                      key={tab}
                      type="button"
                      className={`segmented-item${enhanceTab === tab ? " active" : ""}`}
                      aria-pressed={enhanceTab === tab}
                      onClick={() => setEnhanceTab(tab)}
                    >
                      {tab === "dm" ? t("wecom.enhanceDm") : t("wecom.enhanceGroup")}
                    </button>
                  ))}
                </div>
              </div>
              <label className="toggle" title={t("wecom.enhanceEnabled")}>
                <input
                  type="checkbox"
                  checked={enhanceEnabled}
                  disabled={busy}
                  onChange={(e) => {
                    if (enhanceTab === "dm") {
                      void push({ wecomEnhanceDmEnabled: e.target.checked });
                    } else {
                      void push({ wecomEnhanceGroupEnabled: e.target.checked });
                    }
                  }}
                />
                <span className="toggle-slider" />
              </label>
            </div>

            <p className="setting-hint wecom-enhance-lead">{t("wecom.enhanceEnabledHint")}</p>

            <div className="wecom-editor-stack">
              <textarea
                className="input-field"
                rows={8}
                value={enhanceValue}
                placeholder={
                  enhanceTab === "dm"
                    ? t("wecom.enhanceDmPlaceholder")
                    : t("wecom.enhanceGroupPlaceholder")
                }
                onFocus={() => {
                  if (enhanceTab === "dm") enhanceDm.focused.current = true;
                  else enhanceGroup.focused.current = true;
                }}
                onChange={(e) => {
                  const next = Array.from(e.target.value).slice(0, 2000).join("");
                  if (enhanceTab === "dm") enhanceDm.setValue(next);
                  else enhanceGroup.setValue(next);
                }}
                onBlur={() => {
                  if (enhanceTab === "dm") {
                    enhanceDm.focused.current = false;
                    const v = enhanceDm.value.trim();
                    if (v !== (settings.wecomEnhanceDm ?? "").trim()) {
                      void push({ wecomEnhanceDm: v });
                    }
                  } else {
                    enhanceGroup.focused.current = false;
                    const v = enhanceGroup.value.trim();
                    if (v !== (settings.wecomEnhanceGroup ?? "").trim()) {
                      void push({ wecomEnhanceGroup: v });
                    }
                  }
                }}
              />
              <div className="wecom-editor-foot">
                <div className="wecom-enhance-actions">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => {
                      const sample =
                        enhanceTab === "dm"
                          ? t("wecom.enhanceDmSample")
                          : t("wecom.enhanceGroupSample");
                      const clipped = Array.from(sample).slice(0, 2000).join("");
                      if (enhanceTab === "dm") {
                        enhanceDm.setValue(clipped);
                        void push({ wecomEnhanceDm: clipped.trim() });
                      } else {
                        enhanceGroup.setValue(clipped);
                        void push({ wecomEnhanceGroup: clipped.trim() });
                      }
                    }}
                  >
                    {t("wecom.enhanceFillSample")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => {
                      if (enhanceTab === "dm") {
                        enhanceDm.setValue("");
                        void push({ wecomEnhanceDm: "" });
                      } else {
                        enhanceGroup.setValue("");
                        void push({ wecomEnhanceGroup: "" });
                      }
                    }}
                  >
                    {t("wecom.enhanceClear")}
                  </button>
                </div>
                <span className="wecom-char-count">
                  {t("wecom.charCount", { count: enhanceCount })}
                </span>
              </div>
            </div>

            <p className="setting-hint wecom-enhance-note">{t("wecom.enhanceHint")}</p>
          </div>

          {/* 会话与共享任务：纯行为说明（会话怎么建立 / 命令怎么发 / 群里谁能看到 / 会话怎么迁移），
              原在访问策略，2026-11-06 按性质归堆迁到「回答与上下文」尾部——
              连通检查清单一条拆去运行诊断页顶 */}
          <div className="wecom-block-card" hidden={answerView !== "answer"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <ListChecks size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.sessionRules")}</span>
              </div>
            </div>

            <div className="wecom-rules-grid">
              <Hint summary={t("wecom.sessionRulesShort")} details={t("wecom.sessionRulesHint")} />
              <Hint summary={t("wecom.sharedTasksShort")} details={t("wecom.sharedTasksHint")} />
              <Hint summary={t("wecom.sessionCommandsShort")} details={t("wecom.sessionCommandsHint")} />
              <Hint summary={t("wecom.groupVisibilityShort")} details={t("wecom.groupVisibilityHint")} />
              <Hint summary={t("wecom.sessionMigrationShort")} details={t("wecom.sessionMigrationHint")} />
            </div>
          </div>

          {/* 权限档位：这条通道回答时用天玑的哪一档权限（只读 / 工作区 / 审查 / 完全），
              并发上限（全局活动回合上限）也归这卡——两行都管「引擎拿多少能力、
              同时干多少活」。原在「访问策略」，2026-11-06 按性质归堆迁入引擎视图，
              排在引擎与模型之后、工作区之前：先定能力边界，再定在哪干 */}
          <div className="wecom-block-card" hidden={answerView !== "engine"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <ShieldCheck size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subWhat")}</span>
              </div>
            </div>

            {/* 渠道权限与并发上限并排一行（同 wecom-pair 双列，对齐工作区与文档卡） */}
            <div className="wecom-pair">
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
                hint={<Hint summary={t("wecom.maxJobsHintShort")} details={t("wecom.maxJobsHint")} />}
              />
            </div>
          </div>

          {/* 工作区与文档：和上面的增强卡片同一套壳，两个字段并排、说明落到卡片底部 */}
          <div className="wecom-block-card" hidden={answerView !== "engine"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <FolderGit2 size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subDocWorkspace")}</span>
              </div>
            </div>

            <div className="wecom-pair">
              <Row
                label={t("wecom.repoPath")}
                control={
                  <div className="input-with-toggle">
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
                        if (v !== (settings.wecomRepoPath ?? "")) {
                          void push({ wecomRepoPath: v || undefined });
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="input-toggle-btn"
                      title={t("wecom.pickDirectory")}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => void pickAnswerDir("wecomRepoPath")}
                    >
                      <FolderOpen size={14} />
                    </button>
                  </div>
                }
              />

              <Row
                label={t("wecom.docDir")}
                control={
                  <div className="input-with-toggle">
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
                        if (v !== (settings.wecomDocDir ?? "")) {
                          void push({ wecomDocDir: v || undefined });
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="input-toggle-btn"
                      title={t("wecom.pickDirectory")}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => void pickAnswerDir("wecomDocDir")}
                    >
                      <FolderOpen size={14} />
                    </button>
                  </div>
                }
                hint={<p className="setting-hint">{t("wecom.docDirHint")}</p>}
              />
            </div>

            {engine === "dsh" ? (
              <p className="setting-hint wecom-block-note">{t("wecom.dshWorkspaceHint")}</p>
            ) : null}
          </div>

          {/* 线上排障：查日志与调用链的证据源（测试 / 生产各一套 OAP）。
              它决定回答时去哪个 OAP 补查证据，是「怎么答」的一部分，
              所以并入回答设置的第三个二级页签，不再单独占一级页签。 */}
          <div className="wecom-block-card" hidden={answerView !== "trouble"}>
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <Activity size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subTroubleshoot")}</span>
              </div>
              {/* 测试 / 生产切换贴着字段放：换环境就是换这一组地址 */}
              <div
                className="devtools-segmented wecom-head-seg"
                role="group"
                aria-label={t("wecom.skywalkingEnv")}
              >
                {(["test", "prod"] as WecomSkywalkingEnv[]).map((env) => (
                  <button
                    key={env}
                    type="button"
                    className={`segmented-item${skywalkingEnv === env ? " active" : ""}`}
                    disabled={busy || skywalkingEnv === env}
                    aria-pressed={skywalkingEnv === env}
                    onClick={() => {
                      if (skywalkingEnv === env) return;
                      void push(
                        buildWecomSkywalkingPatch(useGlobalStore.getState().settings, { env }),
                      );
                    }}
                  >
                    {env === "test" ? t("wecom.skywalkingEnvTest") : t("wecom.skywalkingEnvProd")}
                  </button>
                ))}
              </div>
            </div>

            <p className="setting-hint wecom-block-lead">{t("wecom.skywalkingEnvHint")}</p>

            {/* 地址与时区并排一行：地址长、时区只是短下拉，按 2:1 分列 */}
            <div className="wecom-pair is-addr">
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
                      if (v !== (skywalkingProfile.addr ?? "").trim())
                        void push(
                          buildWecomSkywalkingPatch(useGlobalStore.getState().settings, {
                            profilePatch: { addr: v || undefined },
                          }),
                        );
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
                    value={skywalkingProfile.timezoneOffsetMinutes ?? ""}
                    disabled={busy}
                    onChange={(e) =>
                      void push(
                        buildWecomSkywalkingPatch(useGlobalStore.getState().settings, {
                          profilePatch: {
                            timezoneOffsetMinutes:
                              e.target.value === "" ? undefined : Number(e.target.value),
                          },
                        }),
                      )
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
            </div>

            <Row
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
                    if (v !== (skywalkingProfile.header ?? "").trim())
                      void push(
                        buildWecomSkywalkingPatch(useGlobalStore.getState().settings, {
                          profilePatch: { header: v || undefined },
                        }),
                      );
                  }}
                />
              }
            />
          </div>
        </div>
      </div>


      {/* 5 · 访问控制：会话准入（私聊 / 群聊 / 配对授权）。
          2026-11-06 权限档位也迁「回答设置 → 引擎与模型」后，本页只剩准入一页，
          不再需要 who/what 二级页签；配对是准入的落地动作，跟着白名单放。 */}
      <div className="net-panel" hidden={section !== "access"}>
        <PanelHead title={t("wecom.groupAccess")} />
        <div className="wecom-rows">
          {/* 私聊：谁能私聊 + 私聊白名单 */}
          <div className="wecom-block-card is-split">
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <MessageSquare size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subDmAccess")}</span>
              </div>
            </div>

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
          </div>

          {/* 群聊：谁能进群 + 群白名单 + 是否必须 @ */}
          <div className="wecom-block-card is-split">
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <Users size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.subGroupAccess")}</span>
              </div>
            </div>

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
          </div>

          {/* 权限档位卡已迁「回答设置 → 引擎与模型」（2026-11-06）；
              会话与共享任务说明卡迁「回答与上下文」，连通检查清单迁「运行诊断」页顶。
              本页现在只剩会话准入三张卡。 */}

          <div className="wecom-block-card">
            <div className="wecom-block-head">
              <div className="wecom-block-head-main">
                <span className="wecom-block-icon" aria-hidden>
                  <UserCheck size={14} />
                </span>
                <span className="wecom-block-title">{t("wecom.groupPairing")}</span>
              </div>
              {(settings.wecomPairedUsers ?? []).length > 0 ? (
                <span className="wecom-count-badge">
                  {(settings.wecomPairedUsers ?? []).length}
                </span>
              ) : pairings.length > 0 ? (
                <span className="wecom-count-badge is-pending">{pairings.length}</span>
              ) : null}
            </div>

            {pairings.length === 0 ? (
              <div className="wecom-empty-state">
                <span className="wecom-empty-state-icon" aria-hidden>
                  <UserCheck size={18} />
                </span>
                <p>{t("wecom.pairingEmpty")}</p>
              </div>
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
        {/* 连通检查清单：原在访问策略的会话规则卡里，2026-11-06 按性质拆到诊断页顶——
            「连不上时按清单自查」正是本页的入口动作 */}
        <div className="wecom-diag-block wecom-diag-checklist">
          <Hint summary={t("wecom.connectChecklistShort")} details={t("wecom.connectChecklistHint")} />
        </div>
        <WecomRequestDiagnostics selection={diagnosticSelection} />
        <section className="wecom-diag-block">
          <div className="wecom-diag-head">
            <div className="wecom-diag-head-main">
              <span className="wecom-diag-icon" aria-hidden>
                <Gauge size={13} />
              </span>
              <span>{t("wecom.diag.performance")}</span>
            </div>
          </div>
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
                          : t("wecom.diag.notReported"),
                      })}</span>
                    </div>
                    <p className="setting-hint">{t("wecom.diag.modelReported", { model: performanceSummary.model || t("wecom.diag.notReported"), effort: reportedEffort(performanceSummary.effort) })}</p>
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
                  </div>
                </details>
              ) : null}
            </>
          ) : <p className="setting-hint">{t("wecom.diag.notReported")}</p>}
          <p className="setting-hint wecom-diag-meta">
            {t("wecom.diag.cacheHits", { countText: reportedCount(investigation?.cacheHits) })}
            {investigation?.requestId ? <> · {t("wecom.diag.requestId")} <code>{investigation.requestId}</code></> : null}
          </p>
        </section>
        {/* 两块日志降一层：先给分组标题，块标题再弱化，免得和上面三次诊断挤在同一层次 */}
        <div className="wecom-subhead wecom-diag-subhead">{t("wecom.diag.rawLogs")}</div>
        <div className="wecom-diag-grid">
          <section className="wecom-diag-block">
            <div className="wecom-diag-head">
              <div className="wecom-diag-head-main">
                <span className="wecom-diag-icon" aria-hidden>
                  <Terminal size={13} />
                </span>
                <span>{t("wecom.diag.channelLog")}</span>
              </div>
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
              <div className="wecom-diag-head-main">
                <span className="wecom-diag-icon" aria-hidden>
                  <MessagesSquare size={13} />
                </span>
                <span>{t("wecom.diag.sessionLog")}</span>
              </div>
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
          </section>
        </div>
      </div>

      {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
    </div>
  );
}
