import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
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
  Activity,
  Info,
} from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import GroupedSelect from "./GroupedSelect";
import ModalTitleRow from "./ModalTitleRow";
import WecomBotRail from "./WecomBotRail";
import WecomBotHero from "./WecomBotHero";
import WecomChannelPage from "./WecomChannelPage";
import { useGlobalStore } from "../core/store";
import type {
  AppSettings,
  DshModelCatalog,
  WecomBotProfile,
  WecomSkywalkingEnv,
} from "../core/types";
import {
  applyWecomChannel,
  buildWecomSkywalkingPatch,
  protectSecret,
  revealWecomSecret,
  approveWecomPairing,
  fetchDshModels,
  fetchWecomPairings,
  fetchWecomStatus,
  fetchWecomWorkspaceSyncStatus,
  normalizeWecomMaxJobs,
  resolveWecomSkywalking,
  restartWecomChannel,
  revokeWecomUser,
  startWecomQr,
  pollWecomQr,
  setWecomWorkspaceSyncEnabled,
  syncDshProfile,
  type WecomBotRuntimeStatus,
  type WecomWorkspaceSyncStatus,
  type WecomPairing,
  type WecomStatus,
} from "../lib/wecomChannel";
import {
  buildAddWecomBotPatch,
  buildPurgeIncompleteWecomBotsPatch,
  buildRemoveWecomBotPatch,
  buildSelectWecomBotPatch,
  clampWecomBotRemark,
  createEmptyWecomBot,
  foldWecomFlatPatch,
  getActiveWecomBot,
  listBoundWecomBots,
  wecomBotLabel,
} from "../lib/wecomBots";

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

/**
 * 页面内部的小卡片 Tab：点谁只看谁，一屏放得下。
 * 「通道」已并进右栏身份头，「运行诊断」是全通道数据、挂在通道汇总页；
 * 这里只剩两组按机器人隔离的配置。
 */
type WecomSection = "answer" | "access";

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
  const [revealedSecret, setRevealedSecret] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  // DSH 引擎的模型目录（ACP configOptions），按引擎就绪懒加载
  const [catalog, setCatalog] = useState<DshModelCatalog | null>(null);
  const [catalogBusy, setCatalogBusy] = useState(false);
  const [catalogError, setCatalogError] = useState("");
  /** DSH 工作区实时同步（后端读取桌面端 patch 是否已注入，不落本应用设置） */
  const [workspaceSync, setWorkspaceSync] = useState<WecomWorkspaceSyncStatus | null>(null);
  const [workspaceSyncBusy, setWorkspaceSyncBusy] = useState(false);
  /** 小卡片 Tab：一页只显示一组设置，别把六七块叠成一条长滚动 */
  const [section, setSection] = useState<WecomSection>("answer");
  /** 右栏看的对象：某台机器人（主从）还是通道汇总（全部机器人） */
  const [pane, setPane] = useState<"bot" | "channel">("bot");
  const [qrBusy, setQrBusy] = useState(false);
  const [qrScode, setQrScode] = useState("");
  const [qrAuthUrl, setQrAuthUrl] = useState("");
  const [qrSvg, setQrSvg] = useState("");
  const [qrPageUrl, setQrPageUrl] = useState("");
  const [qrDetail, setQrDetail] = useState("");
  /** 通道：编辑弹框。create 草稿不进 wecomBots，有 Bot ID 点完成才入库。 */
  const [botEditOpen, setBotEditOpen] = useState(false);
  const [botEditMode, setBotEditMode] = useState<"create" | "edit">("edit");
  const [botEditLocalId, setBotEditLocalId] = useState("");
  const [botEditForm, setBotEditForm] = useState({
    remark: "",
    displayName: "",
    botId: "",
    secret: "",
    corpId: "",
    corpSecret: "",
  });
  const qrPollRef = useRef<number | null>(null);
  const mounted = useRef(false);
  const refreshGeneration = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      refreshGeneration.current += 1;
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
      const current = useGlobalStore.getState().settings;
      patch = foldWecomFlatPatch(current, patch);
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
        setRevealedSecret("");
        setShowSecret(false);
      }
      await refresh();
      setMsg(t("wecom.saved"));
    } catch (e) {
      if (enabling) setSettings(foldWecomFlatPatch(useGlobalStore.getState().settings, { wecomEnabled: false }));
      setMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleSecret = async () => {
    if (showSecret) {
      setShowSecret(false);
      return;
    }
    if (
      botEditOpen &&
      botEditMode === "edit" &&
      !botEditForm.secret.trim() &&
      settings.wecomBotSecret
    ) {
      try {
        const plain = await revealWecomSecret(settings.wecomBotSecret);
        setRevealedSecret(plain);
        setBotEditForm((prev) => ({ ...prev, secret: plain }));
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

  // —— 主从布局的作用域收敛 ——
  // 顶层 status.state / engine / connectedMs / lastError 来自「第一个在线实例」，
  // 只有一台机器人时才允许兜底；多机器人下一律取这台机器人自己的运行态。
  const bots = listBoundWecomBots(settings);
  const activeBot = getActiveWecomBot(settings);
  const runtimeByBotId: Record<string, WecomBotRuntimeStatus | undefined> = {};
  for (const item of status?.bots ?? []) {
    if (item.botId) runtimeByBotId[item.botId] = item;
  }
  const singleBot = bots.length === 1;
  const activeRuntime = activeBot?.botId ? runtimeByBotId[activeBot.botId] : undefined;
  const activeDsh = activeRuntime?.dsh ?? (singleBot ? status?.dsh : undefined) ?? null;
  const fallbackState = singleBot ? status?.state : undefined;
  const stateKey =
    (activeBot?.enabled ? (activeRuntime?.state ?? fallbackState) : undefined) ?? "disconnected";
  const stateLabel = t(`wecom.state.${stateKey}`, { defaultValue: stateKey });
  const engine = (activeRuntime?.engine ?? settings.wecomEngine) === "dsh" ? "dsh" : "tianji";
  const dshState = activeDsh?.state ?? "stopped";
  const dshLabel = t(`wecom.dsh.state.${dshState}`, { defaultValue: dshState });
  const dshReady = dshState === "ready";
  // 待审批是逐机器人的（后端 wecom_pairing_list 带 bot_id）；没有 bot_id 的旧条目不计入任何一台
  const pendingByBotId: Record<string, number> = {};
  for (const pair of pairings) {
    const key = (pair.botId ?? "").trim();
    if (!key) continue;
    pendingByBotId[key] = (pendingByBotId[key] ?? 0) + 1;
  }
  const activePending = activeBot?.botId ? (pendingByBotId[activeBot.botId] ?? 0) : 0;
  // 待审批列表也只列这台机器人的：没有 bot_id 的旧条目仍展示，否则永远批不掉
  const activePairings = pairings.filter(
    (pair) => !pair.botId || pair.botId === activeBot?.botId,
  );

  // DSH 镜像按机器人各存一份（每台一个 ACP profile），不能沿用顶层主实例的
  const mirror = activeDsh?.mirror ?? null;

  const loadModels = useCallback(async (refresh: boolean, scope?: "channel" | "docgen") => {
    setCatalogBusy(true);
    setCatalogError("");
    try {
      setCatalog(await fetchDshModels(refresh, scope, settings.wecomBotId));
    } catch (e) {
      setCatalogError(String(e));
    } finally {
      setCatalogBusy(false);
    }
  }, [settings.wecomBotId]);

  // 换机器人就丢掉上一台的模型目录：DSH 模型是按 bot 的 profile 列的，
  // 沿用会让人以为两台共用一份模型清单。
  const activeBotKey = activeBot?.id ?? "";
  useEffect(() => {
    setCatalog(null);
    setCatalogError("");
  }, [activeBotKey]);

  // 换机器人时把右栏面板滚回顶部：内容整块换人，停在上一台的半截位置最容易被误读
  const detailRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const panel = detailRef.current?.querySelector<HTMLElement>(".net-panel:not([hidden])");
    panel?.scrollTo({ top: 0 });
  }, [activeBotKey]);

  // 引擎就绪后再拉模型目录；DSH 没起来时不主动去启进程
  // （进页面就去 spawn 一个 node 只会让人以为界面卡住）。
  useEffect(() => {
    if (catalog) return;
    if (engine === "dsh" && dshReady) void loadModels(false, "channel");
  }, [engine, dshReady, catalog, loadModels]);

  // 同步开关的状态存在 DSH 配置里（不是本应用设置）：进页面读一次，读不到只让开关不可点。
  useEffect(() => {
    if (engine !== "dsh") return;
    let alive = true;
    void fetchWecomWorkspaceSyncStatus()
      .then((next) => {
        if (alive) setWorkspaceSync(next);
      })
      .catch(() => {
        // 读不到状态不影响本页其它配置。
      });
    return () => {
      alive = false;
    };
  }, [engine]);

  const toggleWorkspaceSync = async (enabled: boolean) => {
    setWorkspaceSyncBusy(true);
    setMsg("");
    try {
      setWorkspaceSync(await setWecomWorkspaceSyncEnabled(enabled));
    } catch (e) {
      setMsg(String(e));
    } finally {
      setWorkspaceSyncBusy(false);
    }
  };

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

  const resetQrUi = () => {
    stopQrPoll();
    setQrBusy(false);
    setQrScode("");
    setQrAuthUrl("");
    setQrSvg("");
    setQrPageUrl("");
    setQrDetail("");
  };

  /** 关掉编辑弹框；create 草稿未入库，直接丢。扫码中关闭=取消扫码。 */
  const closeBotEdit = useCallback(() => {
    if (busy) return;
    resetQrUi();
    setBotEditOpen(false);
    setBotEditLocalId("");
    setBotEditForm({
      remark: "",
      displayName: "",
      botId: "",
      secret: "",
      corpId: "",
      corpSecret: "",
    });
    const purge = buildPurgeIncompleteWecomBotsPatch(useGlobalStore.getState().settings);
    if (purge) setSettings(purge);
  }, [busy, setSettings]);

  const openCreateBot = (startQr: boolean) => {
    const bot = createEmptyWecomBot();
    // 无论从左栏还是通道汇总页进来，接完这台就落到它的配置页
    setPane("bot");
    setBotEditMode("create");
    setBotEditLocalId(bot.id);
    setBotEditForm({
      remark: "",
      displayName: "",
      botId: "",
      secret: "",
      corpId: "",
      corpSecret: "",
    });
    setRevealedSecret("");
    setShowSecret(false);
    setBotEditOpen(true);
    if (startQr) void startQrBind();
  };

  const openEditBot = (bot: { id: string; remark?: string; botDisplayName?: string; botId?: string; corpId?: string }) => {
    setPane("bot");
    setSettings(buildSelectWecomBotPatch(settings, bot.id));
    setBotEditMode("edit");
    setBotEditLocalId(bot.id);
    setBotEditForm({
      remark: bot.remark ?? "",
      displayName: bot.botDisplayName ?? "",
      botId: bot.botId ?? "",
      secret: "",
      corpId: bot.corpId ?? "",
      corpSecret: "",
    });
    setRevealedSecret("");
    setShowSecret(false);
    setBotEditOpen(true);
  };

  /** 完成：create 要有 Bot ID 才入库；edit 写回当前 bot。 */
  const commitBotEdit = async () => {
    if (busy) return;
    const form = botEditForm;
    const id = botEditLocalId;
    const nextBotId = form.botId.trim();
    if (botEditMode === "create" && !nextBotId) {
      closeBotEdit();
      return;
    }
    const remark = clampWecomBotRemark(form.remark);
    const display = form.displayName.trim();
    const nextCorpId = form.corpId.trim();
    const plainSecret = form.secret.trim();
    const plainCorp = form.corpSecret.trim();

    if (botEditMode === "create") {
      const bot = createEmptyWecomBot({
        id,
        botId: nextBotId,
        botDisplayName: display || undefined,
        remark: remark || undefined,
        corpId: nextCorpId || undefined,
        enabled: true,
      });
      const addPatch = buildAddWecomBotPatch(useGlobalStore.getState().settings, bot);
      setSettings(addPatch);
      await push(
        {
          wecomBotId: nextBotId,
          wecomEnabled: true,
          wecomBotDisplayName: display,
          wecomBotRemark: remark,
          wecomCorpId: nextCorpId,
        },
        plainSecret || undefined,
        plainCorp || undefined,
      );
    } else {
      await push(
        {
          wecomBotId: nextBotId,
          wecomBotDisplayName: display,
          wecomBotRemark: remark,
          wecomCorpId: nextCorpId,
        },
        plainSecret || undefined,
        plainCorp || undefined,
      );
    }
    resetQrUi();
    setBotEditOpen(false);
    setBotEditLocalId("");
  };

  // 进页时清掉历史遗留的空壳（未完成扫码/手动接入的条目）
  useEffect(() => {
    const purge = buildPurgeIncompleteWecomBotsPatch(useGlobalStore.getState().settings);
    if (purge) setSettings(purge);
    // 只在挂载时扫一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
              setQrPageUrl("");
              setBotEditForm((prev) => ({
                ...prev,
                botId: polled.botId ?? prev.botId,
                secret: polled.secret ?? prev.secret,
              }));
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

  /** 删除机器人：左栏与右栏身份头共用一份确认与切人逻辑。 */
  const removeBot = async (bot: WecomBotProfile) => {
    const name = wecomBotLabel(bot, t("wecom.botUntitled"));
    const ok = await confirm({
      title: t("wecom.botsRemove"),
      message: t("wecom.botsRemoveConfirm", { name }),
    });
    if (!ok) return;
    const patch = buildRemoveWecomBotPatch(settings, bot.id);
    setSettings(patch);
    if (settings.wecomActiveBotId === bot.id) setBotEditOpen(false);
    void push(patch);
  };

  return (
    <div className="wecom-settings">
      {/* 主从：左栏选机器人，右栏只放这一台的连接身份与配置 */}
      <div className="wecom-master-detail">
        <WecomBotRail
          bots={bots}
          activeId={settings.wecomActiveBotId}
          runtimeByBotId={runtimeByBotId}
          pendingByBotId={pendingByBotId}
          onlineCount={status?.botsOnline ?? (status?.state === "connected" ? 1 : 0)}
          totalCount={status?.botsTotal ?? bots.filter((b) => b.enabled).length}
          busy={busy || qrBusy}
          channelSelected={pane === "channel"}
          onSelectChannel={() => setPane("channel")}
          onSelect={(id) => {
            setPane("bot");
            setSettings(buildSelectWecomBotPatch(settings, id));
          }}
          onEdit={openEditBot}
          onRemove={(bot) => void removeBot(bot)}
          onScanAdd={() => openCreateBot(true)}
          onManualAdd={() => {
            openCreateBot(false);
            setMsg(t("wecom.botsManualHint"));
          }}
        />

        <section className="wecom-bot-detail" ref={detailRef}>
          {pane === "channel" ? (
            <WecomChannelPage
              bots={bots}
              runtimeByBotId={runtimeByBotId}
              status={status}
              busy={busy}
              onRestart={() => {
                setBusy(true);
                void restartWecomChannel()
                  .then((st) => {
                    setStatus(st);
                    setMsg(t("wecom.restarted"));
                  })
                  .catch((e) => setMsg(String(e)))
                  .finally(() => setBusy(false));
              }}
              onOpenBot={(id) => {
                setPane("bot");
                setSettings(buildSelectWecomBotPatch(settings, id));
              }}
            />
          ) : activeBot ? (
            <>
              <WecomBotHero
                bot={activeBot}
                runtime={activeRuntime}
                fallbackState={fallbackState}
                busy={busy}
                onToggleEnabled={(enabled) => void push({ wecomEnabled: enabled })}
                onEdit={() => openEditBot(activeBot)}
                onRemove={() => void removeBot(activeBot)}
              />

              {/* 三页签：一屏只看一组设置；「通道」已并进上面的身份头，
                  作用域由身份头那一行说明，这里不再重复贴机器人名 */}
              <div className="wecom-sections" role="tablist" aria-label={t("wecom.title")}>
                {(
                  [
                    {
                      id: "answer",
                      title: t("wecom.groupAnswer"),
                      summary: engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji"),
                    },
                    {
                      id: "access",
                      title: t("wecom.groupAccess"),
                      summary:
                        activePending > 0
                          ? t("wecom.sectionPairedPending", {
                              count: (settings.wecomPairedUsers ?? []).length,
                              pending: activePending,
                            })
                          : t("wecom.sectionPaired", {
                              count: (settings.wecomPairedUsers ?? []).length,
                            }),
                      attention: activePending > 0,
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
                      className={`wecom-section-summary${
                        "attention" in item && item.attention ? " is-pending" : ""
                      }`}
                    >
                      {item.summary}
                    </span>
                  </button>
                ))}
              </div>
            </>
          ) : (
            <div className="wecom-bot-empty">
              <span className="wecom-bot-empty-icon" aria-hidden>
                <Bot size={22} />
              </span>
              <p className="wecom-bot-empty-title">{t("wecom.botsEmptyTitle")}</p>
              <p className="setting-hint">{t("wecom.botsEmptyHint")}</p>
              <div className="wecom-bot-empty-actions">
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  disabled={busy || qrBusy}
                  onClick={() => openCreateBot(true)}
                >
                  <QrCode size={14} />
                  {t("wecom.botsScanAdd")}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={busy}
                  onClick={() => openCreateBot(false)}
                >
                  {t("wecom.botsManualAdd")}
                </button>
              </div>
            </div>
          )}

          {botEditOpen
          ? createPortal(
              <div
                className="modal-overlay"
                onMouseDown={(e) => {
                  if (e.target === e.currentTarget) closeBotEdit();
                }}
              >
                <div
                  className="modal modal-lg wecom-bot-edit-modal"
                  role="dialog"
                  aria-modal="true"
                  aria-label={t("wecom.botsEditing", {
                    name: wecomBotLabel(
                      {
                        id: botEditLocalId,
                        remark: botEditForm.remark,
                        botDisplayName: botEditForm.displayName,
                        botId: botEditForm.botId,
                      },
                      t("wecom.botUntitled"),
                    ),
                  })}
                >
                  <ModalTitleRow
                    title={t("wecom.botsEditing", {
                      name: wecomBotLabel(
                        {
                          id: botEditLocalId,
                          remark: botEditForm.remark,
                          botDisplayName: botEditForm.displayName,
                          botId: botEditForm.botId,
                        },
                        t("wecom.botUntitled"),
                      ),
                    })}
                    onClose={closeBotEdit}
                    disabled={busy}
                    badge={
                      <span className="card-title-badge">
                        <span
                          className={`wecom-status-pill ${
                            botEditMode === "create"
                              ? qrBusy
                                ? "is-pending"
                                : ""
                              : statusClass(
                                  status?.bots?.find((b) => b.botId === botEditForm.botId)?.state ??
                                    (settings.wecomBotId === botEditForm.botId
                                      ? status?.state
                                      : undefined),
                                )
                          }`}
                        >
                          <i className="wecom-status-dot" aria-hidden />
                          {botEditMode === "create"
                            ? qrBusy
                              ? t("wecom.qrWaiting")
                              : botEditForm.botId.trim()
                                ? t("wecom.sectionOn")
                                : t("wecom.sectionOff")
                            : stateLabel}
                        </span>
                      </span>
                    }
                  />
                  <div className="wecom-bot-edit-body">
                    <div className="wecom-bot-edit-row">
                      <div className="input-group">
                        <label className="input-label">{t("wecom.botRemark")}</label>
                        <input
                          className="input-field"
                          value={botEditForm.remark}
                          placeholder={t("wecom.botRemarkPlaceholder")}
                          maxLength={40}
                          onChange={(e) =>
                            setBotEditForm((prev) => ({ ...prev, remark: e.target.value }))
                          }
                        />
                      </div>
                      <div className="input-group">
                        <label className="input-label">{t("wecom.displayName")}</label>
                        <input
                          className="input-field"
                          value={botEditForm.displayName}
                          placeholder={t("wecom.displayNamePlaceholder")}
                          onChange={(e) =>
                            setBotEditForm((prev) => ({ ...prev, displayName: e.target.value }))
                          }
                        />
                      </div>
                    </div>
                    <div className="wecom-bot-edit-head">
                      <p className="wecom-bot-edit-section">{t("wecom.subBot")}</p>
                      <div className="wecom-bot-rebind">
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          disabled={busy || qrBusy}
                          onClick={() => void startQrBind()}
                        >
                          {qrBusy ? t("wecom.qrWaiting") : t("wecom.botsRebind")}
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
                            onClick={() => resetQrUi()}
                          >
                            {t("wecom.qrCancel")}
                          </button>
                        ) : null}
                      </div>
                    </div>
                    <div className="wecom-bot-edit-row">
                      <div className="input-group">
                        <label className="input-label">{t("wecom.botId")}</label>
                        <input
                          className="input-field"
                          value={botEditForm.botId}
                          placeholder="botId"
                          onChange={(e) =>
                            setBotEditForm((prev) => ({ ...prev, botId: e.target.value }))
                          }
                        />
                      </div>
                      <div className="input-group">
                        <label className="input-label">{t("wecom.botSecret")}</label>
                        <div className="input-with-toggle">
                          <input
                            className="input-field"
                            type={showSecret ? "text" : "password"}
                            value={
                              showSecret
                                ? botEditForm.secret || revealedSecret
                                : botEditForm.secret
                            }
                            placeholder={
                              botEditMode === "edit" && settings.wecomBotSecret && !botEditForm.secret
                                ? showSecret
                                  ? ""
                                  : `••••••••  ${t("wecom.secretKept")}`
                                : t("wecom.secretPlaceholder")
                            }
                            onChange={(e) =>
                              setBotEditForm((prev) => ({ ...prev, secret: e.target.value }))
                            }
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
                          {!qrSvg ? (
                            <p className="setting-hint">{t("wecom.qrSvgMissing")}</p>
                          ) : null}
                          {qrDetail ? <p className="setting-hint">{qrDetail}</p> : null}
                          {qrScode ? (
                            <p className="setting-hint">
                              <code className="tabular-nums">{qrScode.slice(0, 8)}…</code>
                            </p>
                          ) : null}
                        </div>
                      </div>
                    ) : null}
                    <p className="wecom-bot-edit-section">{t("wecom.subCorp")}</p>
                    <div className="wecom-bot-edit-row">
                      <div className="input-group">
                        <label className="input-label">{t("wecom.corpId")}</label>
                        <input
                          className="input-field"
                          value={botEditForm.corpId}
                          placeholder={t("wecom.corpIdPlaceholder")}
                          onChange={(e) =>
                            setBotEditForm((prev) => ({ ...prev, corpId: e.target.value }))
                          }
                        />
                      </div>
                      <div className="input-group">
                        <label className="input-label">{t("wecom.corpSecret")}</label>
                        <input
                          className="input-field"
                          type="password"
                          value={botEditForm.corpSecret}
                          placeholder={
                            botEditMode === "edit" &&
                            settings.wecomCorpSecret &&
                            !botEditForm.corpSecret
                              ? `••••••••  ${t("wecom.secretKept")}`
                              : t("wecom.corpSecretPlaceholder")
                          }
                          onChange={(e) =>
                            setBotEditForm((prev) => ({ ...prev, corpSecret: e.target.value }))
                          }
                        />
                        <p className="wecom-tip" title={t("wecom.corpHint")}>
                          <Info size={12} aria-hidden />
                          {t("wecom.corpTip")}
                        </p>
                      </div>
                    </div>
                  </div>
                  <div className="modal-actions">
                    <button
                      type="button"
                      className="btn btn-secondary"
                      disabled={busy}
                      onClick={closeBotEdit}
                    >
                      {t("wecom.botsEditCancel")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={busy || qrBusy}
                      onClick={() => void commitBotEdit()}
                    >
                      {t("wecom.botsEditDone")}
                    </button>
                  </div>
                </div>
              </div>,
              document.body,
            )
          : null}

      {/* 2 · 回答设置：谁来回答、按什么口径回答、在哪个工作区回答 */}
      <div className="net-panel" hidden={pane === "channel" || !activeBot || section !== "answer"}>
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

            {/* 会话实时同步：DSH 的工作区表只在宿主启动时读一次，不注入这个插件
                就得重启 Harness 才能看到企微新建的会话。开关即注入/卸载。 */}
            {engine === "dsh" ? (
              <Row
                asSwitch
                label={t("wecom.workspaceSync")}
                control={
                  <label className="toggle" title={t("wecom.workspaceSyncHintShort")}>
                    <input
                      type="checkbox"
                      checked={Boolean(workspaceSync?.installed)}
                      disabled={busy || workspaceSyncBusy || workspaceSync === null}
                      onChange={(e) => void toggleWorkspaceSync(e.target.checked)}
                    />
                    <span className="toggle-slider" />
                  </label>
                }
                hint={
                  <>
                    <Hint
                      summary={t("wecom.workspaceSyncHintShort")}
                      details={t("wecom.workspaceSyncHint")}
                    />
                    {workspaceSync?.installed && !workspaceSync.active ? (
                      <p className="setting-hint">{t("wecom.workspaceSyncPending")}</p>
                    ) : null}
                    {workspaceSync?.detail ? (
                      <p className="setting-hint">{workspaceSync.detail}</p>
                    ) : null}
                  </>
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
      <div className="net-panel" hidden={pane === "channel" || !activeBot || section !== "access"}>
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
              ) : activePairings.length > 0 ? (
                <span className="wecom-count-badge is-pending">{activePairings.length}</span>
              ) : null}
            </div>

            {activePairings.length === 0 ? (
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
                {activePairings.map((p) => (
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
        </section>
      </div>


      {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
    </div>
  );
}
