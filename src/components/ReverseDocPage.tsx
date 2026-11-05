/**
 * 逆向工程（从「IM 机器人 → 文档自动更新与推送」整块拎出来的独立页）。
 *
 * 三条主线：多久跑一次（调度）→ 读哪些仓、怎么写（更新内容）→ 跑完怎么提交与通知（执行与推送）。
 *
 * 配置仍存在 settings（`wecomDocgen*` / `wecomWebhook*`），下发走 `wecom_docgen_apply`：
 * 这条路径只写后端的文档配置快照，不校验 bot 凭据、不重启企微长连接，
 * 因此跟「IM 机器人」页互不影响（两边从同一份设置取值，收敛到同一个 DocgenConfig）。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Check, Clock, Eye, EyeOff, FolderGit2, Play, RefreshCw, Send, Sparkles } from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import GroupedSelect from "./GroupedSelect";
import { useTauriEvent } from "../hooks/useTauriEvent";
import { useGlobalStore } from "../core/store";
import type { AppSettings, DshModelCatalog, GitScannedRepo } from "../core/types";
import { invocations } from "../core/store/invocations";
import {
  DEFAULT_DOCGEN_QUALITY_PROMPT,
  DEFAULT_DOCGEN_SYSTEM_PROMPT,
  DOCGEN_PROMPT_CHAR_CAP,
  WECOM_DOCGEN_PROGRESS_EVENT,
  applyWecomDocgen,
  fetchDshModels,
  fetchWecomDocgenHistory,
  fetchWecomDocgenProgress,
  normalizeDocgenPrompt,
  normalizeDocgenRepoAllowlist,
  resetWecomDocgenCursor,
  runWecomDocgenNow,
  testWecomDocgen,
  type WecomDocgenHistoryItem,
  type WecomDocgenProgress,
} from "../lib/wecomChannel";

function pathKey(p: string): string {
  return p.trim().replace(/\//g, "\\").toLowerCase();
}

/** 进度分段：与后端 `progress.stage(...)` 的阶段名对齐 */
const DOCGEN_STEPS = ["collect", "write", "quality", "commit", "notify"] as const;
const DOCGEN_PHASE_STEP: Record<string, number> = {
  workspace: 0,
  status: 0,
  branch: 0,
  sync: 0,
  collect: 0,
  agent: 1,
  quality: 2,
  preview: 3,
  commit: 3,
  publish: 3,
  notify: 4,
  cursor: 4,
  done: DOCGEN_STEPS.length,
};
function docgenStepIndex(phase: string): number {
  return DOCGEN_PHASE_STEP[phase] ?? -1;
}

/** 跟随已保存设置、除非正在输入的受控输入框。 */
function useFieldDraft(saved: string) {
  const [value, setValue] = useState(saved);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current) setValue(saved);
  }, [saved]);
  return { value, setValue, focused };
}

/** 字段说明：默认只露一句摘要，长文案收进「说明 ⌄」。 */
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

/** 一行：字段名在上、控件在下；`asSwitch` 换成标签在左、开关在右。 */
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

/** 按码点截断（不切坏 emoji）；UTF-16 长度没超就当没超，省掉逐字符遍历。 */
function clampPrompt(value: string): string {
  if (value.length <= DOCGEN_PROMPT_CHAR_CAP) return value;
  let i = 0;
  let n = 0;
  while (i < value.length && n < DOCGEN_PROMPT_CHAR_CAP) {
    const code = value.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < value.length) {
      const next = value.charCodeAt(i + 1);
      i += next >= 0xdc00 && next <= 0xdfff ? 2 : 1;
    } else {
      i += 1;
    }
    n += 1;
  }
  return value.slice(0, i);
}

/** 码点计数：不建临时数组，提示词上万字符时也别在每次击键上分配。 */
function promptCharCount(value: string): number {
  let n = 0;
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < value.length) {
      const next = value.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) i += 1;
    }
    n += 1;
  }
  return n;
}

/** 毫秒 → 「x 秒 / x 分 x 秒 / x 时 x 分 x 秒」，进度卡与历史表共用。 */
function formatDuration(t: TFunction<"settings">, ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return t("wecom.durHms", { h, m, s });
  if (m > 0) return t("wecom.durMs", { m, s });
  return t("wecom.durS", { s });
}

/**
 * 提示词卡（更新 / 质检两个模板共用一个编辑器）。
 *
 * 草稿状态留在卡片内部：打字时只有这张卡重渲染，不会把整页一起带着重算。
 */
function DocgenPromptCard({
  hidden,
  tab,
  onTabChange,
  system,
  quality,
  savedSystem,
  savedQuality,
  busy,
  qualityCheck,
  onToggleQualityCheck,
  onSavePrompt,
}: {
  /** 二级页签切换时整卡隐藏（组件仍挂载，草稿切回来还在） */
  hidden?: boolean;
  tab: "system" | "quality";
  onTabChange: (tab: "system" | "quality") => void;
  /** 已生效的模板（未自定义时就是内置默认） */
  system: string;
  quality: string;
  /** 已保存的原始值（空串 = 用默认） */
  savedSystem: string;
  savedQuality: string;
  busy: boolean;
  qualityCheck: boolean;
  onToggleQualityCheck: (next: boolean) => void;
  /** 传 undefined 表示回到内置默认 */
  onSavePrompt: (which: "system" | "quality", value: string | undefined) => void;
}) {
  const { t } = useTranslation("settings");
  const isSystem = tab === "system";
  const systemDraft = useFieldDraft(system);
  const qualityDraft = useFieldDraft(quality);
  const draft = isSystem ? systemDraft : qualityDraft;
  const count = useMemo(() => promptCharCount(draft.value), [draft.value]);
  const defaultPrompt = isSystem ? DEFAULT_DOCGEN_SYSTEM_PROMPT : DEFAULT_DOCGEN_QUALITY_PROMPT;
  const saved = isSystem ? savedSystem : savedQuality;

  const commit = () => {
    draft.focused.current = false;
    const v = normalizeDocgenPrompt(draft.value);
    if (v === normalizeDocgenPrompt(saved)) return;
    onSavePrompt(tab, v && v !== normalizeDocgenPrompt(defaultPrompt) ? v : undefined);
  };

  return (
    <div className="wecom-block-card wecom-docgen-prompt-card" hidden={hidden}>
      <div className="wecom-block-head">
        <div className="wecom-block-head-main">
          <span className="wecom-block-icon" aria-hidden>
            <Sparkles size={14} />
          </span>
          <span className="wecom-block-title">{t("wecom.subDocgenPrompts")}</span>
          <div
            className="devtools-segmented wecom-head-seg"
            role="group"
            aria-label={t("wecom.subDocgenPrompts")}
          >
            {(["system", "quality"] as const).map((item) => (
              <button
                key={item}
                type="button"
                className={`segmented-item${tab === item ? " active" : ""}`}
                aria-pressed={tab === item}
                onClick={() => onTabChange(item)}
              >
                {item === "system"
                  ? t("wecom.docgenSystemPrompt")
                  : t("wecom.docgenQualityPrompt")}
              </button>
            ))}
          </div>
        </div>
      </div>

      <p className="setting-hint wecom-block-lead">
        {isSystem ? t("wecom.docgenSystemPromptHint") : t("wecom.docgenQualityPromptHint")}
      </p>

      <div className="wecom-editor-stack">
        <textarea
          className="input-field"
          rows={12}
          value={draft.value}
          disabled={busy}
          onFocus={() => {
            draft.focused.current = true;
          }}
          onChange={(e) => draft.setValue(clampPrompt(e.target.value))}
          onBlur={commit}
        />
        <div className="wecom-editor-foot">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy}
            onClick={() => {
              draft.setValue(defaultPrompt);
              onSavePrompt(tab, undefined);
            }}
          >
            {t("wecom.docgenPromptReset")}
          </button>
          <span className="wecom-char-count">
            {t("wecom.docgenPromptCharCount", { count, cap: DOCGEN_PROMPT_CHAR_CAP })}
          </span>
        </div>
      </div>

      <Row
        asSwitch
        label={t("wecom.docgenQualityCheck")}
        control={
          <label className="toggle">
            <input
              type="checkbox"
              checked={qualityCheck}
              onChange={(e) => onToggleQualityCheck(e.target.checked)}
            />
            <span className="toggle-slider" />
          </label>
        }
        hint={<p className="setting-hint">{t("wecom.docgenQualityCheckHint")}</p>}
      />
    </div>
  );
}

export default function ReverseDocPage() {
  const { t } = useTranslation("settings");
  const confirm = useConfirm();
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);

  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  /** 二级页签：调度 / 更新内容 / 执行与推送 */
  const [docgenView, setDocgenView] = useState<"schedule" | "content" | "publish" | "record">(
    "schedule",
  );
  const [docgenPromptTab, setDocgenPromptTab] = useState<"system" | "quality">("system");
  const [docgenBusy, setDocgenBusy] = useState(false);
  const [docgenHistory, setDocgenHistory] = useState<WecomDocgenHistoryItem[]>([]);
  const [docgenHistoryBusy, setDocgenHistoryBusy] = useState(false);
  const [docgenProgress, setDocgenProgress] = useState<WecomDocgenProgress | null>(null);
  const [docgenScanBusy, setDocgenScanBusy] = useState(false);
  const [docgenResetBusy, setDocgenResetBusy] = useState(false);
  // null = 尚未成功扫描；[] = 扫描成功但没有子仓。两者不能混为一谈。
  const [docgenScanned, setDocgenScanned] = useState<GitScannedRepo[] | null>(null);
  const [docgenScanRoot, setDocgenScanRoot] = useState("");
  // DSH 引擎的模型目录（ACP configOptions），按需懒加载
  const [catalog, setCatalog] = useState<DshModelCatalog | null>(null);
  const [catalogBusy, setCatalogBusy] = useState(false);
  const [catalogError, setCatalogError] = useState("");
  // Webhook 默认隐藏（地址里带 access token），点眼睛按需查看
  const [showWebhook, setShowWebhook] = useState(false);
  const [showWebhookFeishu, setShowWebhookFeishu] = useState(false);
  const [showWebhookDingtalk, setShowWebhookDingtalk] = useState(false);
  // 运行中的实时计时：每秒更新一次「当前时间」，渲染时拿它减开始时间；结束后用 durationMs
  const [nowMs, setNowMs] = useState(0);

  const docgenRepoPath = useFieldDraft(settings.wecomDocgenRepoPath ?? "");
  const docgenDocDir = useFieldDraft(settings.wecomDocgenDocDir ?? "");
  const docgenCron = useFieldDraft(settings.wecomDocgenCron ?? "");
  const releaseBranch = useFieldDraft(settings.wecomReleaseBranch ?? "");
  const autoUpdateTime = useFieldDraft(settings.wecomAutoUpdateTime ?? "");
  const webhookDraft = useFieldDraft(settings.wecomWebhookUrl ?? "");
  const webhookFeishuDraft = useFieldDraft(settings.wecomWebhookFeishu ?? "");
  const webhookDingtalkDraft = useFieldDraft(settings.wecomWebhookDingtalk ?? "");

  /** 存设置 + 只下发文档配置（不走通道 apply，不会重启长连接）。 */
  const push = async (next: Partial<AppSettings> = {}) => {
    setBusy(true);
    setMsg("");
    try {
      if (Object.keys(next).length) setSettings(next);
      const merged = { ...useGlobalStore.getState().settings, ...next };
      await applyWecomDocgen(merged);
      setMsg(t("wecom.saved"));
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** 回话引擎（`inherit` 时文档更新跟着它走）。 */
  const engine = settings.wecomEngine === "dsh" ? "dsh" : "tianji";
  const docgenEngine =
    settings.wecomDocgenEngine === "dsh" || settings.wecomDocgenEngine === "tianji"
      ? settings.wecomDocgenEngine
      : "inherit";
  const docgenEngineResolved = docgenEngine === "inherit" ? engine : docgenEngine;
  const lookbackValue = (() => {
    const n = settings.wecomDocgenLookbackDays;
    return typeof n === "number" && Number.isFinite(n)
      ? Math.max(0, Math.min(30, Math.trunc(n)))
      : 7;
  })();
  const lookbackChoices = useMemo(() => {
    const base = [0, 3, 7, 14, 30];
    return base.includes(lookbackValue)
      ? base
      : [...base, lookbackValue].sort((a, b) => a - b);
  }, [lookbackValue]);
  /** 文档更新留空时实际沿用的 DSH 模型名（下拉里直接写出来，省得猜）。 */
  const inheritedDshModel = (() => {
    const v = (settings.wecomDshModel ?? "").trim();
    if (!v) return t("wecom.dshModelDefault");
    const hit = (catalog?.groups ?? [])
      .flatMap((group) => group.options)
      .find((option) => option.value === v);
    return hit?.name || v;
  })();
  const docgenEngineHint =
    docgenEngine === "inherit"
      ? t("wecom.docgenEngineInheritHint", {
          engine: engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji"),
        })
      : docgenEngine === "dsh"
        ? t("wecom.docgenEngineDshHint")
        : t("wecom.docgenEngineTianjiHint");
  /** 文档更新的生效工作区：自己填了就用自己那份，否则才回落到「回答设置」。 */
  const docgenWorkspace = (docgenRepoPath.value || settings.wecomDocgenRepoPath || "").trim();
  const docgenInheritedWorkspace = (settings.wecomRepoPath || settings.currentGitRepo || "").trim();
  /** 文档目录留空时沿用的值（「回答设置」的文档目录，再空就是 doc）。 */
  const docgenInheritedDocDir = (settings.wecomDocDir ?? "").trim();
  /** 扫描子仓的根目录（白名单本来就是在它下面勾）。 */
  const docgenScanBase = (docgenWorkspace || docgenInheritedWorkspace).trim();
  /** 运行条上的「下次运行」：cron 优先，其次每日时刻。 */
  const docgenScheduleLabel = settings.wecomDocgenCron?.trim()
    ? t("wecom.docgenCronSummary", { cron: settings.wecomDocgenCron.trim() })
    : t("wecom.sectionDailyAt", { time: settings.wecomAutoUpdateTime || "23:00" });

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

  // 联动：更新引擎（显式选的或沿用的）落到 DSH 时自动拉一次模型目录。
  // 失败不自动重试——未配工作区时后端会给出可操作文案，显示在下拉提示里即可；
  // 想再试可以点「刷新模型」，那条路径每次都会清掉旧错误。
  useEffect(() => {
    if (catalog || catalogError) return;
    if (docgenEngineResolved === "dsh") void loadModels(false, "docgen");
  }, [docgenEngineResolved, catalog, catalogError, loadModels]);

  // 进页面：补一次更新记录 + 从后端快照恢复进度卡（那一轮可能还在跑）
  useEffect(() => {
    void fetchWecomDocgenHistory()
      .then(setDocgenHistory)
      .catch(() => {});
    void fetchWecomDocgenProgress()
      .then(({ progress, running }) => {
        // 按钮锁只看后端真实状态，别被一份过期的 "running" 事件永久禁掉
        setDocgenBusy(running);
        if (progress) setDocgenProgress((prev) => prev ?? progress);
      })
      .catch(() => {});
  }, []);

  useTauriEvent<WecomDocgenProgress>(WECOM_DOCGEN_PROGRESS_EVENT, (event) => {
    const progress = event.payload;
    // 同内容不重渲染（进度事件可能连发几条）
    setDocgenProgress((prev) =>
      prev &&
      prev.runId === progress.runId &&
      prev.status === progress.status &&
      prev.phase === progress.phase &&
      prev.percent === progress.percent &&
      prev.detail === progress.detail
        ? prev
        : progress,
    );
    if (progress.status === "running") {
      setDocgenBusy(true);
      return;
    }
    setDocgenBusy(false);
    void fetchWecomDocgenHistory()
      .then(setDocgenHistory)
      .catch(() => {});
  });

  // 运行中每秒跳一次，让「已运行」实时走动；结束 / 页面切走时定时器自动撤掉
  useEffect(() => {
    if (docgenProgress?.status !== "running" || !docgenProgress.startedAtMs) return;
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [docgenProgress?.status, docgenProgress?.startedAtMs]);

  return (
    <div className="settings-page is-solo">
      <div className="card">
        {/* 四段页签占卡片标题位：配置（调度 / 内容 / 提交推送）与结果（运行记录）分开 */}
        <div className="card-title is-tabs">
          <div
            className="hub-tabs is-channel"
            role="tablist"
            aria-label={t("wecom.groupAutoupdate")}
          >
            {(["schedule", "content", "publish", "record"] as const).map((view) => (
              <button
                key={view}
                type="button"
                role="tab"
                aria-selected={docgenView === view}
                className={`hub-tab${docgenView === view ? " is-active" : ""}`}
                onClick={() => setDocgenView(view)}
              >
                {t(`wecom.docgenView.${view}`)}
                {view === "record" && docgenView !== "record" && docgenHistory.length > 0 ? (
                  <span className="hub-tab-count">{docgenHistory.length}</span>
                ) : null}
              </button>
            ))}
          </div>
        </div>
        <div className="card-body">
          {/* 常驻运行条：启停 + 下次运行 + 两个动作。切到任何页签都能直接跑、直接看结果 */}
          <div className="wecom-docgen-runbar">
            <div className="wecom-bind">
              <div className="wecom-bind-main">
                <span className="wecom-bind-icon" aria-hidden>
                  <Play size={16} />
                </span>
                <div className="wecom-bind-text">
                  <span className="wecom-bind-title">
                    {settings.wecomAutoUpdateEnabled
                      ? t("wecom.docgenNextRun", { schedule: docgenScheduleLabel })
                      : t("wecom.docgenRunbarOff")}
                  </span>
                  <p className="setting-hint">
                    {docgenScanBase ? t("wecom.docgenRunNowHint") : t("wecom.docgenRunNeedWorkspace")}
                  </p>
                </div>
                <label className="toggle" title={t("wecom.autoUpdateEnabled")}>
                  <input
                    type="checkbox"
                    checked={Boolean(settings.wecomAutoUpdateEnabled)}
                    onChange={(e) => void push({ wecomAutoUpdateEnabled: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <div className="wecom-bind-actions">
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
                  className="btn btn-primary btn-small"
                  disabled={docgenBusy || !docgenScanBase}
                  onClick={() => {
                    setDocgenBusy(true);
                    setMsg("");
                    setDocgenProgress({
                      runId: "pending",
                      trigger: settings.wecomDocgenPreview ? "preview" : "manual",
                      phase: "workspace",
                      percent: 1,
                      status: "running",
                      detail: "",
                    });
                    void runWecomDocgenNow()
                      .then((r) => {
                        setMsg(r);
                        void fetchWecomDocgenHistory()
                          .then(setDocgenHistory)
                          .catch(() => {});
                      })
                      .catch((e) => {
                        const detail = String(e);
                        setMsg(detail);
                        setDocgenProgress((prev) => ({
                          runId: prev?.runId ?? "manual-error",
                          trigger: prev?.trigger ?? "manual",
                          phase: "fail",
                          percent: 100,
                          status: "fail",
                          detail,
                        }));
                      })
                      .finally(() => setDocgenBusy(false));
                  }}
                >
                  {docgenBusy ? t("wecom.docgenRunning") : t("wecom.docgenRunNowBtn")}
                </button>
              </div>
            </div>
            {docgenProgress && docgenProgress.status && docgenProgress.phase
              ? (() => {
                  const progress = docgenProgress;
                  const pct = Math.max(0, Math.min(100, Math.round(progress.percent)));
                  const step = docgenStepIndex(progress.phase);
                  return (
                    <div
                      className={`wecom-docgen-progress is-${progress.status}`}
                      role="status"
                      aria-live="polite"
                    >
                      <div className="wecom-docgen-progress-head">
                        <span className="wecom-docgen-progress-pulse" aria-hidden />
                        <span className="wecom-docgen-progress-label">
                          {t(`wecom.docgenPhase.${progress.phase}`, {
                            defaultValue: progress.phase,
                          })}
                        </span>
                        <span className="wecom-docgen-progress-meta">
                          {t(`wecom.docgenTrigger.${progress.trigger}`, {
                            defaultValue: progress.trigger,
                          })}
                        </span>
                        {/* 耗时：运行中拿 startedAtMs 实时算，结束用后端给的 durationMs */}
                        {progress.status === "running" && progress.startedAtMs ? (
                          <span className="wecom-docgen-progress-elapsed">
                            {t("wecom.docgenElapsedRunning", {
                              time: formatDuration(
                                t,
                                Math.max(0, (nowMs || Date.now()) - progress.startedAtMs),
                              ),
                            })}
                          </span>
                        ) : progress.durationMs ? (
                          <span className="wecom-docgen-progress-elapsed">
                            {t("wecom.docgenElapsedTotal", {
                              time: formatDuration(t, progress.durationMs),
                            })}
                          </span>
                        ) : null}
                        <span className="wecom-docgen-progress-pct">
                          {pct}
                          <i>%</i>
                        </span>
                      </div>
                      {/* 五段能量条：一段一个阶段；已过的段点亮，正在跑的段流斜纹 */}
                      <div
                        className="wecom-docgen-segbar"
                        role="progressbar"
                        aria-valuemin={0}
                        aria-valuemax={100}
                        aria-valuenow={pct}
                      >
                        {DOCGEN_STEPS.map((key, index) => {
                          const segPct = Math.max(
                            0,
                            Math.min(100, Math.round((pct - index * 20) * 5)),
                          );
                          const state =
                            step > index ? "is-done" : step === index ? "is-current" : "";
                          return (
                            <div
                              key={key}
                              className={`wecom-docgen-seg${state ? ` ${state}` : ""}`}
                            >
                              <div className="wecom-docgen-seg-track">
                                <div
                                  className="wecom-docgen-seg-fill"
                                  style={{ width: `${segPct}%` }}
                                />
                              </div>
                              <span className="wecom-docgen-seg-name">
                                {t(`wecom.docgenStep.${key}`)}
                              </span>
                            </div>
                          );
                        })}
                      </div>
                      {progress.detail ? (
                        <p className="wecom-docgen-progress-detail">{progress.detail}</p>
                      ) : null}
                    </div>
                  );
                })()
              : null}
          </div>
          {/* 动作反馈紧跟运行条：点「测试 / 立即更新」时眼睛就在第一屏，别让结果滚到卡底 */}
          {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
          <div className="wecom-rows">
            {/* 1. 调度与触发 */}
            <div className="wecom-block-card is-split" hidden={docgenView !== "schedule"}>
              <div className="wecom-block-head">
                <div className="wecom-block-head-main">
                  <span className="wecom-block-icon" aria-hidden>
                    <RefreshCw size={14} />
                  </span>
                  <span className="wecom-block-title">{t("wecom.subDocgenSchedule")}</span>
                </div>
              </div>

              {/* 开关在页头常驻运行条上，这里只留说明与启用前检查清单 */}
              <Hint summary={t("wecom.autoUpdateHintShort")} details={t("wecom.autoUpdateHint")} />
              <p className="setting-hint">{t("wecom.docgenChecklist")}</p>

              <Row
                label={t("wecom.docgenEngineLabel")}
                control={
                  <select
                    className="input-field wecom-split"
                    value={docgenEngine}
                    disabled={busy}
                    onChange={(e) => void push({ wecomDocgenEngine: e.target.value })}
                  >
                    <option value="inherit">
                      {t("wecom.docgenEngineInherit", {
                        engine: engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji"),
                      })}
                    </option>
                    <option value="tianji">{t("wecom.engineTianji")}</option>
                    <option value="dsh">{t("wecom.engineDsh")}</option>
                  </select>
                }
                hint={<p className="setting-hint">{docgenEngineHint}</p>}
              />

              {/* DSH 档才有模型/思考强度：留空沿用「回答设置」的 DSH 选择 */}
              {docgenEngineResolved === "dsh" ? (
                <Row
                  label={t("wecom.dshModelRow")}
                  control={
                    <>
                      <GroupedSelect
                        className="wecom-split"
                        value={settings.wecomDocgenDshModel ?? ""}
                        disabled={busy || catalogBusy}
                        fallbackLabel={t("wecom.docgenDshInheritValue", {
                          model: inheritedDshModel,
                        })}
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
                        onChange={(next) => void push({ wecomDocgenDshModel: next })}
                      />
                      <select
                        className="input-field wecom-split-effort"
                        value={settings.wecomDocgenDshEffort ?? ""}
                        disabled={busy || catalogBusy || !catalog?.effort}
                        onChange={(e) => void push({ wecomDocgenDshEffort: e.target.value })}
                      >
                        <option value="">{t("wecom.docgenDshInherit")}</option>
                        {(catalog?.effort?.options ?? [])
                          .filter((option) => Boolean(option.value))
                          .map((option) => (
                            <option key={option.value} value={option.value}>
                              {option.name || option.value}
                            </option>
                          ))}
                      </select>
                      <button
                        type="button"
                        className="btn btn-secondary btn-small wecom-split-refresh"
                        disabled={busy || catalogBusy}
                        onClick={() => void loadModels(true, "docgen")}
                      >
                        {catalogBusy ? t("wecom.dshLoading") : t("wecom.dshRefreshModels")}
                      </button>
                    </>
                  }
                  hint={
                    <p className="setting-hint">
                      {catalogError
                        ? catalogError
                        : catalog
                          ? t("wecom.docgenDshModelHint")
                          : t("wecom.docgenDshModelNeedReady")}
                    </p>
                  }
                />
              ) : null}

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
                label={t("wecom.docgenCron")}
                control={
                  <input
                    className="input-field"
                    value={docgenCron.value}
                    placeholder={t("wecom.docgenCronPlaceholder")}
                    onFocus={() => {
                      docgenCron.focused.current = true;
                    }}
                    onChange={(e) => docgenCron.setValue(e.target.value)}
                    onBlur={() => {
                      docgenCron.focused.current = false;
                      const v = docgenCron.value.trim();
                      if (v !== (settings.wecomDocgenCron ?? ""))
                        void push({ wecomDocgenCron: v || undefined });
                    }}
                  />
                }
                hint={<p className="setting-hint">{t("wecom.docgenCronHint")}</p>}
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
                asSwitch
                label={t("wecom.docgenPollRelease")}
                control={
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={Boolean(settings.wecomDocgenPollRelease)}
                      onChange={(e) => void push({ wecomDocgenPollRelease: e.target.checked })}
                    />
                    <span className="toggle-slider" />
                  </label>
                }
                hint={<p className="setting-hint">{t("wecom.docgenPollReleaseHint")}</p>}
              />
            </div>

            {/* 2. 变更来源 */}
            <div className="wecom-block-card" hidden={docgenView !== "content"}>
              <div className="wecom-block-head">
                <div className="wecom-block-head-main">
                  <span className="wecom-block-icon" aria-hidden>
                    <FolderGit2 size={14} />
                  </span>
                  <span className="wecom-block-title">{t("wecom.subDocgenSources")}</span>
                </div>
                <div className="wecom-allowlist-actions">
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy || docgenScanBusy}
                    onClick={() => {
                      const root = docgenScanBase;
                      if (!root) {
                        setDocgenScanned(null);
                        setMsg(t("wecom.docgenRepoRootMissing"));
                        return;
                      }
                      setMsg("");
                      setDocgenScanBusy(true);
                      void invocations
                        .invokeGitScanRepos(root, 1)
                        .then((rows) => {
                          setDocgenScanned(rows ?? []);
                          setDocgenScanRoot(root);
                        })
                        .catch((e) => {
                          setDocgenScanned(null);
                          setMsg(String(e));
                        })
                        .finally(() => setDocgenScanBusy(false));
                    }}
                  >
                    {docgenScanBusy ? t("wecom.docgenRepoScanning") : t("wecom.docgenRepoScan")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy || (docgenScanned?.length ?? 0) === 0}
                    onClick={() => {
                      const paths = normalizeDocgenRepoAllowlist(
                        (docgenScanned ?? []).map((r) => r.path),
                      );
                      void push({ wecomDocgenRepoAllowlist: paths.length ? paths : undefined });
                    }}
                  >
                    {t("wecom.docgenRepoSelectAll")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy || !(settings.wecomDocgenRepoAllowlist?.length)}
                    onClick={() => void push({ wecomDocgenRepoAllowlist: undefined })}
                  >
                    {t("wecom.docgenRepoClear")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy || docgenResetBusy || docgenBusy}
                    onClick={() => {
                      const selected = normalizeDocgenRepoAllowlist(settings.wecomDocgenRepoAllowlist);
                      const days = settings.wecomDocgenLookbackDays ?? 7;
                      void (async () => {
                        const ok = await confirm({
                          title: t("wecom.docgenResetCursor"),
                          message:
                            selected.length > 0
                              ? t("wecom.docgenResetCursorConfirm", {
                                  count: selected.length,
                                  days,
                                })
                              : t("wecom.docgenResetCursorConfirmAll", { days }),
                        });
                        if (!ok) return;
                        setMsg("");
                        setDocgenResetBusy(true);
                        try {
                          const result = await resetWecomDocgenCursor(
                            selected.length ? selected : undefined,
                          );
                          // 重置后不能再挂着上一轮的「已跳过」——后端快照也一并清了，
                          // 这里同步清掉本地的，免得看着像重置没生效。
                          setDocgenProgress(null);
                          setMsg(result);
                          void fetchWecomDocgenHistory()
                            .then(setDocgenHistory)
                            .catch(() => {});
                        } catch (e) {
                          setMsg(String(e));
                        } finally {
                          setDocgenResetBusy(false);
                        }
                      })();
                    }}
                  >
                    {t("wecom.docgenResetCursor")}
                  </button>
                </div>
              </div>
              {/* 文档更新自己的工作区：与「回答设置」的工作区互相独立，改一边不影响另一边 */}
              <Row
                label={t("wecom.docgenRepoPath")}
                control={
                  <input
                    className="input-field"
                    value={docgenRepoPath.value}
                    placeholder={docgenInheritedWorkspace || t("wecom.repoPlaceholder")}
                    onFocus={() => {
                      docgenRepoPath.focused.current = true;
                    }}
                    onChange={(e) => docgenRepoPath.setValue(e.target.value)}
                    onBlur={() => {
                      docgenRepoPath.focused.current = false;
                      const v = docgenRepoPath.value.trim();
                      if (v === (settings.wecomDocgenRepoPath ?? "")) return;
                      // 换了工作区，之前扫出来的子仓就不再可信
                      setDocgenScanned(null);
                      void push({ wecomDocgenRepoPath: v || undefined });
                    }}
                  />
                }
                hint={<p className="setting-hint">{t("wecom.docgenRepoPathHint")}</p>}
              />

              {/* 文档目录同样各配一套：改问答那边不会把定时任务的写入位置带走 */}
              <Row
                label={t("wecom.docgenDocDir")}
                control={
                  <input
                    className="input-field"
                    value={docgenDocDir.value}
                    placeholder={docgenInheritedDocDir || t("wecom.docgenDocDirDefault")}
                    onFocus={() => {
                      docgenDocDir.focused.current = true;
                    }}
                    onChange={(e) => docgenDocDir.setValue(e.target.value)}
                    onBlur={() => {
                      docgenDocDir.focused.current = false;
                      const v = docgenDocDir.value.trim();
                      if (v === (settings.wecomDocgenDocDir ?? "")) return;
                      void push({ wecomDocgenDocDir: v || undefined });
                    }}
                  />
                }
                hint={<p className="setting-hint">{t("wecom.docgenDocDirHint")}</p>}
              />

              <p className="setting-hint wecom-block-lead">{t("wecom.docgenRepoAllowlistHint")}</p>
              {/* 采集前同步：只 fetch + 快进，不改本地提交 */}
              <Row
                asSwitch
                label={t("wecom.docgenSyncSources")}
                control={
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={Boolean(settings.wecomDocgenSyncSources)}
                      onChange={(e) => void push({ wecomDocgenSyncSources: e.target.checked })}
                    />
                    <span className="toggle-slider" />
                  </label>
                }
                hint={
                  <Hint
                    summary={t("wecom.docgenSyncSourcesHintShort")}
                    details={t("wecom.docgenSyncSourcesHint")}
                  />
                }
              />
              {/* 补跑上限：只在「新加的仓」或「停跑超过 N 天」时才参与计算，日常按采集点走 */}
              <Row
                label={t("wecom.docgenLookbackDays")}
                control={
                  <select
                    className="input-field wecom-split"
                    value={String(lookbackValue)}
                    disabled={busy}
                    onChange={(e) =>
                      void push({ wecomDocgenLookbackDays: Number(e.target.value) })
                    }
                  >
                    {lookbackChoices.map((n) => (
                      <option key={n} value={n}>
                        {n === 0
                          ? t("wecom.docgenLookbackUnlimited")
                          : t("wecom.docgenLookbackDaysValue", { count: n })}
                      </option>
                    ))}
                  </select>
                }
                hint={
                  <Hint
                    summary={t("wecom.docgenLookbackDaysHintShort")}
                    details={t("wecom.docgenLookbackDaysHint")}
                  />
                }
              />
              <div className="wecom-allowlist">
                {(() => {
                  const selected = normalizeDocgenRepoAllowlist(settings.wecomDocgenRepoAllowlist);
                  const selectedKeys = new Set(selected.map(pathKey));
                  const root = docgenScanBase;
                  const scanVerified =
                    docgenScanned !== null && Boolean(root) && pathKey(docgenScanRoot) === pathKey(root);
                  const scanned = scanVerified ? (docgenScanned ?? []) : [];
                  const byKey = new Map(scanned.map((r) => [pathKey(r.path), r]));
                  const orphan = selected.filter((p) => !byKey.has(pathKey(p)));
                  const rows: {
                    path: string;
                    name: string;
                    missing: boolean;
                    unverified: boolean;
                  }[] = [
                    ...scanned.map((r) => ({
                      path: r.path,
                      name: r.name,
                      missing: false,
                      unverified: false,
                    })),
                    ...orphan.map((p) => ({
                      path: p,
                      name: p.replace(/\\/g, "/").split("/").pop() || p,
                      missing: scanVerified,
                      unverified: !scanVerified,
                    })),
                  ];
                  if (rows.length === 0) {
                    return <p className="setting-hint">{t("wecom.docgenRepoEmpty")}</p>;
                  }
                  return (
                    <ul className="wecom-allowlist-list">
                      {rows.map((row) => {
                        const checked = selectedKeys.has(pathKey(row.path));
                        return (
                          <li
                            key={pathKey(row.path)}
                            className={`wecom-allowlist-item${
                              row.missing ? " is-missing" : row.unverified ? " is-unverified" : ""
                            }`}
                          >
                            <label>
                              <input
                                type="checkbox"
                                checked={checked}
                                disabled={busy}
                                onChange={(e) => {
                                  const next = e.target.checked
                                    ? normalizeDocgenRepoAllowlist([...selected, row.path])
                                    : selected.filter((p) => pathKey(p) !== pathKey(row.path));
                                  void push({
                                    wecomDocgenRepoAllowlist: next.length ? next : undefined,
                                  });
                                }}
                              />
                              <span className="wecom-allowlist-name">{row.name}</span>
                              {row.missing || row.unverified ? (
                                <span className="wecom-allowlist-badge">
                                  {row.missing
                                    ? t("wecom.docgenRepoMissing")
                                    : t("wecom.docgenRepoUnverified")}
                                </span>
                              ) : null}
                            </label>
                          </li>
                        );
                      })}
                    </ul>
                  );
                })()}
              </div>
            </div>

            {/* 3. 提示词：更新 | 质检 分段单编辑器（草稿状态收在卡片内部，打字不牵动整页） */}
            <DocgenPromptCard
              hidden={docgenView !== "content"}
              tab={docgenPromptTab}
              onTabChange={setDocgenPromptTab}
              system={
                settings.wecomDocgenSystemPrompt?.trim()
                  ? settings.wecomDocgenSystemPrompt
                  : DEFAULT_DOCGEN_SYSTEM_PROMPT
              }
              quality={
                settings.wecomDocgenQualityPrompt?.trim()
                  ? settings.wecomDocgenQualityPrompt
                  : DEFAULT_DOCGEN_QUALITY_PROMPT
              }
              savedSystem={settings.wecomDocgenSystemPrompt ?? ""}
              savedQuality={settings.wecomDocgenQualityPrompt ?? ""}
              busy={busy}
              qualityCheck={Boolean(settings.wecomDocgenQualityCheck)}
              onToggleQualityCheck={(next) => void push({ wecomDocgenQualityCheck: next })}
              onSavePrompt={(which, value) =>
                void push(
                  which === "system"
                    ? { wecomDocgenSystemPrompt: value }
                    : { wecomDocgenQualityPrompt: value },
                )
              }
            />

            {/* 4. 提交与推送（含预览） */}
            <div className="wecom-block-card" hidden={docgenView !== "publish"}>
              <div className="wecom-block-head">
                <div className="wecom-block-head-main">
                  <span className="wecom-block-icon" aria-hidden>
                    <Send size={14} />
                  </span>
                  <span className="wecom-block-title">{t("wecom.subDocPublish")}</span>
                </div>
              </div>

              <div className="wecom-subhead">{t("wecom.subPublishChannels")}</div>
              <Row
                label={t("wecom.webhookUrl")}
                control={
                  <div className="input-with-toggle">
                    <input
                      className="input-field wecom-webhook-input"
                      type={showWebhook ? "text" : "password"}
                      value={webhookDraft.value}
                      title={showWebhook ? webhookDraft.value || undefined : undefined}
                      spellCheck={false}
                      placeholder={t("wecom.webhookPlaceholder")}
                      onFocus={() => {
                        webhookDraft.focused.current = true;
                      }}
                      onChange={(e) => webhookDraft.setValue(e.target.value)}
                      onBlur={() => {
                        webhookDraft.focused.current = false;
                        const v = webhookDraft.value.trim();
                        if (v !== (settings.wecomWebhookUrl ?? "").trim()) {
                          void push({ wecomWebhookUrl: v });
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="input-toggle-btn"
                      title={showWebhook ? t("wecom.hideWebhook") : t("wecom.showWebhook")}
                      aria-pressed={showWebhook}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => setShowWebhook((v) => !v)}
                    >
                      {showWebhook ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                  </div>
                }
                hint={<Hint summary={t("wecom.webhookHintShort")} details={t("wecom.webhookHint")} />}
              />

              <Row
                label={t("wecom.webhookFeishu")}
                control={
                  <div className="input-with-toggle">
                    <input
                      className="input-field wecom-webhook-input"
                      type={showWebhookFeishu ? "text" : "password"}
                      value={webhookFeishuDraft.value}
                      title={showWebhookFeishu ? webhookFeishuDraft.value || undefined : undefined}
                      spellCheck={false}
                      placeholder={t("wecom.webhookFeishuPlaceholder")}
                      onFocus={() => {
                        webhookFeishuDraft.focused.current = true;
                      }}
                      onChange={(e) => webhookFeishuDraft.setValue(e.target.value)}
                      onBlur={() => {
                        webhookFeishuDraft.focused.current = false;
                        const v = webhookFeishuDraft.value.trim();
                        if (v !== (settings.wecomWebhookFeishu ?? "").trim()) {
                          void push({ wecomWebhookFeishu: v });
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="input-toggle-btn"
                      title={showWebhookFeishu ? t("wecom.hideWebhook") : t("wecom.showWebhook")}
                      aria-pressed={showWebhookFeishu}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => setShowWebhookFeishu((v) => !v)}
                    >
                      {showWebhookFeishu ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                  </div>
                }
              />

              <Row
                label={t("wecom.webhookDingtalk")}
                control={
                  <div className="input-with-toggle">
                    <input
                      className="input-field wecom-webhook-input"
                      type={showWebhookDingtalk ? "text" : "password"}
                      value={webhookDingtalkDraft.value}
                      title={showWebhookDingtalk ? webhookDingtalkDraft.value || undefined : undefined}
                      spellCheck={false}
                      placeholder={t("wecom.webhookDingtalkPlaceholder")}
                      onFocus={() => {
                        webhookDingtalkDraft.focused.current = true;
                      }}
                      onChange={(e) => webhookDingtalkDraft.setValue(e.target.value)}
                      onBlur={() => {
                        webhookDingtalkDraft.focused.current = false;
                        const v = webhookDingtalkDraft.value.trim();
                        if (v !== (settings.wecomWebhookDingtalk ?? "").trim()) {
                          void push({ wecomWebhookDingtalk: v });
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="input-toggle-btn"
                      title={showWebhookDingtalk ? t("wecom.hideWebhook") : t("wecom.showWebhook")}
                      aria-pressed={showWebhookDingtalk}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => setShowWebhookDingtalk((v) => !v)}
                    >
                      {showWebhookDingtalk ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                  </div>
                }
              />

              <div className="wecom-subhead">{t("wecom.subPublishAction")}</div>

              <Row
                asSwitch
                label={t("wecom.docgenPreview")}
                control={
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={Boolean(settings.wecomDocgenPreview)}
                      onChange={(e) => void push({ wecomDocgenPreview: e.target.checked })}
                    />
                    <span className="toggle-slider" />
                  </label>
                }
                hint={<p className="setting-hint">{t("wecom.docgenPreviewHint")}</p>}
              />

              <Row
                asSwitch
                label={t("wecom.autoCommit")}
                control={
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={settings.wecomAutoCommit !== false}
                      disabled={Boolean(settings.wecomDocgenPreview)}
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
                      disabled={settings.wecomAutoCommit === false || Boolean(settings.wecomDocgenPreview)}
                      onChange={(e) => void push({ wecomAutoPush: e.target.checked })}
                    />
                    <span className="toggle-slider" />
                  </label>
                }
                hint={<p className="setting-hint">{t("wecom.autoPushHint")}</p>}
              />

            </div>

            {/* 5. 运行记录：独立成一页，表很宽，挤在「提交与推送」下面会把配置压扁 */}
            <div className="wecom-block-card" hidden={docgenView !== "record"}>
              <div className="wecom-block-head">
                <div className="wecom-block-head-main">
                  <span className="wecom-block-icon" aria-hidden>
                    <Clock size={14} />
                  </span>
                  <span className="wecom-block-title">{t("wecom.docgenHistory")}</span>
                </div>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={docgenHistoryBusy}
                  onClick={() => {
                    setDocgenHistoryBusy(true);
                    void fetchWecomDocgenHistory()
                      .then(setDocgenHistory)
                      .catch((e) => setMsg(String(e)))
                      .finally(() => setDocgenHistoryBusy(false));
                  }}
                >
                  {t("wecom.docgenHistoryRefresh")}
                </button>
              </div>
              {docgenHistory.length === 0 ? (
                <p className="setting-hint wecom-note">{t("wecom.docgenHistoryEmpty")}</p>
              ) : (
                <div className="wecom-diag-table-wrap">
                  <table className="wecom-docgen-table">
                    <thead>
                      <tr>
                        <th className="is-time">{t("wecom.docgenHistoryColAt")}</th>
                        <th className="is-trigger">{t("wecom.docgenHistoryColTrigger")}</th>
                        <th className="is-state">{t("wecom.docgenHistoryColStatus")}</th>
                        <th className="is-num">{t("wecom.docgenHistoryColDuration")}</th>
                        <th className="is-num">{t("wecom.docgenHistoryColFiles")}</th>
                        <th className="is-num">{t("wecom.docgenHistoryColPush")}</th>
                        <th>{t("wecom.docgenHistoryColSummary")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {docgenHistory.map((row) => {
                        const summary = row.updatedDocs || row.error || "";
                        const ok = row.status === "ok";
                        const failed = row.status === "fail";
                        return (
                          <tr key={`${row.at}-${row.trigger}-${row.branch}`}>
                            <td className="is-time mono">{row.at}</td>
                            <td className="is-trigger">
                              {t(`wecom.docgenTrigger.${row.trigger}`, {
                                defaultValue: row.trigger,
                              })}
                            </td>
                            <td className="is-state">
                              <span
                                className={`wecom-docgen-state${ok ? " is-ok" : failed ? " is-fail" : ""}`}
                              >
                                {ok
                                  ? t("wecom.docgenStateOk")
                                  : failed
                                    ? t("wecom.docgenStateFail")
                                    : row.status}
                              </span>
                            </td>
                            <td className="is-num">
                              {row.durationMs ? (
                                formatDuration(t, row.durationMs)
                              ) : (
                                <span className="wecom-docgen-dash" aria-hidden>
                                  —
                                </span>
                              )}
                            </td>
                            <td className="is-num mono">{row.changedFiles}</td>
                            <td className="is-num">
                              {row.pushed ? (
                                <Check size={12} className="wecom-docgen-check" aria-label="pushed" />
                              ) : (
                                <span className="wecom-docgen-dash" aria-hidden>
                                  —
                                </span>
                              )}
                            </td>
                            <td className="wecom-docgen-summary">
                              {summary ? (
                                <span className="wecom-docgen-summary-text" title={summary}>
                                  {summary}
                                </span>
                              ) : (
                                <span className="wecom-docgen-dash" aria-hidden>
                                  —
                                </span>
                              )}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
