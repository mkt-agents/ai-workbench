import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Check, XCircle, Loader2, Settings2, Database, Info, ListTree } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type { AppTheme } from "../core/types";
import {
  APP_RELEASES_URL,
  checkForUpdatesMultiSource as checkForUpdates,
  clearSkippedVersion,
  getSkippedVersion,
} from "../lib/version";
import {
  CUSTOM_SKIN,
  PANDA_SKIN,
  PET_CONFIG_EVENT,
  PET_ORDER,
  PET_SIZES,
  loadUserPack,
  normalizePetConfig,
  type PetConfig,
  type PetConfigPatch,
} from "../lib/petSkins";
import PetPackImport from "./PetPackImport";
import { applyDocumentTheme, APP_THEME_CHANGED_EVENT } from "../lib/theme";
import {
  clearAiUsage,
  readAiUsageDay,
  usageList,
  readAiUsageHistory,
  type AiUsageDay,
  type AiUsageEntry,
  type AiUsageHistoryPoint,
} from "../lib/aiUsage";
import { useConfirm } from "./ConfirmModal";
import MenuManager from "./MenuManager";
import { emit } from "@tauri-apps/api/event";

const THEME_OPTIONS: AppTheme[] = ["ice", "silver", "glass", "light", "dark", "system"];

/** 走势图 / 明细共用的模型配色（tokens.css 的 --chart-1..6，主题无关的数据可视化色板）。 */
const CHART_COLORS = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
  "var(--chart-6)",
];

type SettingsTab = "general" | "menu" | "data" | "about";

/**
 * 近 14 天 token 用量的迷你柱状图。刻度取「最高的一天」和「每日预算」里大的那个，
 * 所以设了预算就能一眼看出哪几天越线（越线的柱子变红，虚线是预算）。
 */
/** Catmull-Rom → cubic Bézier：把折点串成平滑曲线（首尾用端点回退处理）。 */
function smoothPath(pts: { x: number; y: number }[]): string {
  if (pts.length < 2) return "";
  if (pts.length < 3) {
    return `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)} L ${pts
      .slice(1)
      .map((p) => `${p.x.toFixed(2)} ${p.y.toFixed(2)}`)
      .join(" L ")}`;
  }
  let d = `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)}`;
  // 控制点夹在 [0,100]：陡峭跳变（连续 0 后来一个峰值）会把控制点甩出基线，
  // 曲线跟着「穿底」。贝塞尔不出控制点凸包，夹住控制点就夹住了整条曲线。
  const clampY = (y: number) => Math.min(100, Math.max(0, y));
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const c1x = p1.x + (p2.x - p0.x) / 6;
    const c1y = clampY(p1.y + (p2.y - p0.y) / 6);
    const c2x = p2.x - (p3.x - p1.x) / 6;
    const c2y = clampY(p2.y - (p3.y - p1.y) / 6);
    d += ` C ${c1x.toFixed(2)} ${c1y.toFixed(2)}, ${c2x.toFixed(2)} ${c2y.toFixed(2)}, ${p2.x.toFixed(
      2
    )} ${p2.y.toFixed(2)}`;
  }
  return d;
}

function UsageTrend({
  days,
  budget,
  series,
}: {
  days: AiUsageHistoryPoint[];
  budget: number;
  series: { id: string; name: string; color: string }[];
}) {
  const { t } = useTranslation("settings");
  const [mode, setMode] = useState<"bars" | "line">("bars");
  const valueOf = (d: AiUsageHistoryPoint, id: string) => d.byModel?.[id] ?? 0;
  const max = Math.max(budget, ...days.map((d) => d.tokens), 1);
  const today = days[days.length - 1]?.date ?? "";
  const peak = Math.max(...days.map((d) => d.tokens), 0);
  const tip = (d: AiUsageHistoryPoint) =>
    t("usage.trendTip", { date: d.date.slice(5), tokens: d.tokens.toLocaleString(), calls: d.calls });
  return (
    <div className="usage-trend">
      <div className="usage-trend-head">
        <span className="usage-trend-head-left">
          <span className="usage-trend-title">{t("usage.trendTitle")}</span>
          {peak > 0 && (
            <span className="usage-trend-peak">{t("usage.trendPeak", { tokens: peak.toLocaleString() })}</span>
          )}
        </span>
        <span className="usage-trend-switch" role="tablist" aria-label={t("usage.trendTitle")}>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "bars"}
            className={`usage-trend-switch-btn${mode === "bars" ? " is-active" : ""}`}
            onClick={() => setMode("bars")}
          >
            {t("usage.trendBars")}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "line"}
            className={`usage-trend-switch-btn${mode === "line" ? " is-active" : ""}`}
            onClick={() => setMode("line")}
          >
            {t("usage.trendLine")}
          </button>
        </span>
      </div>
      <div className="usage-trend-plot">
        {budget > 0 && (
          <div className="usage-trend-budget" style={{ bottom: `${(budget / max) * 100}%` }} />
        )}
        {mode === "bars"
          ? days.map((d) => {
              // 真实比例，不再 Math.max(6,…) 夸大小用量天；0 值天由 CSS min-height 兜底可见
              const height = Math.round((d.tokens / max) * 100);
              const over = budget > 0 && d.tokens > budget;
              return (
                <div key={d.date} className="usage-trend-col" title={tip(d)}>
                  <i
                    className={`usage-trend-bar${d.tokens === 0 ? " is-empty" : ""}${over ? " is-over" : ""}${d.date === today ? " is-today" : ""}`}
                    style={{ height: `${height}%` }}
                  >
                    {series.map((s) => {
                      const v = valueOf(d, s.id);
                      // 堆叠段用 flexGrow 分：各模型的 token 占这条柱的比例
                      return v > 0 ? <b key={s.id} style={{ flexGrow: v, background: s.color }} /> : null;
                    })}
                  </i>
                </div>
              );
            })
          : (
              <svg className="usage-trend-line is-area" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden>
                {(series.length > 0
                  ? series.map((s) => ({
                      key: s.id,
                      color: s.color,
                      value: (d: AiUsageHistoryPoint) => valueOf(d, s.id),
                    }))
                  : // 旧版本记的历史没有 byModel：柱状有总量、曲线没得分模型——退回画总量一条线
                    [
                      {
                        key: "total",
                        color: "var(--accent-strong)",
                        value: (d: AiUsageHistoryPoint) => d.tokens,
                      },
                    ]
                ).map((c) => {
                  // 曲线折点：x 落在每根柱的正中，y 按同一刻度，0 掉到基线
                  const pts = days.map((d, i) => ({
                    x: ((i + 0.5) / days.length) * 100,
                    y: 100 - Math.min((c.value(d) / max) * 100, 100),
                  }));
                  const path = smoothPath(pts);
                  return (
                    <g key={c.key}>
                      <path
                        d={`${path} L 100 100 L 0 100 Z`}
                        style={{ fill: `color-mix(in srgb, ${c.color} 12%, transparent)`, stroke: "none" }}
                      />
                      <path d={path} style={{ stroke: c.color }} />
                    </g>
                  );
                })}
                {days.map((d, i) => (
                  <rect key={d.date} x={(i / days.length) * 100} y="0" width={100 / days.length} height="100" fill="transparent">
                    <title>{tip(d)}</title>
                  </rect>
                ))}
              </svg>
            )}
      </div>
      <div className="usage-trend-days">
        {days.map((d) => (
          <span key={d.date} className={d.date === today ? "is-today" : undefined}>
            {d.date.slice(8)}
          </span>
        ))}
      </div>
    </div>
  );
}

/**
 * 左栏分类。卡片自己带 `hidden={tab !== id}`，切换时只有 `display` 变——
 * 挂载状态保住了，输入框里的半截草稿不会被切走就丢。
 *
 * 「天玑助手」（交互/能力/预算）和「联网扩展」（抓取/MCP/API）已经搬到侧栏的
 * 「天玑助手」分组里，这里只剩通用 / 菜单 / 用量 / 关于。
 */
const SETTINGS_TABS: { id: SettingsTab; icon: typeof Settings2; labelKey: string }[] = [
  // 外观（通用 + 菜单）→ 数据 → 关于；「天玑助手」「联网扩展」已搬到侧栏的「天玑助手」分组
  { id: "general", icon: Settings2, labelKey: "tabs.general" },
  { id: "menu", icon: ListTree, labelKey: "tabs.menu" },
  { id: "data", icon: Database, labelKey: "tabs.data" },
  { id: "about", icon: Info, labelKey: "tabs.about" },
];

type Props = {
  /** 手动检查发现新版：挂右下角「发现新版本」浮框（与启动自动检查同一条）。 */
  onNoticeUpdate?: (target: { version: string; htmlUrl: string }) => void;
};

function Settings({ onNoticeUpdate }: Props = {}) {
  const { t, i18n } = useTranslation("settings");
  const confirm = useConfirm();

  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  // 桌宠三项都可能缺省（旧版只写 localStorage 的 qa-bubble-pet），统一按归一化结果渲染。
  const petCfg = useMemo(
    () =>
      normalizePetConfig({
        skin: settings.aiHubPetSkin,
        size: settings.aiHubPetSize,
        quiet: settings.aiHubPetQuiet,
      }),
    [settings.aiHubPetSkin, settings.aiHubPetSize, settings.aiHubPetQuiet],
  );
  /** 自备素材包变了才需要桌宠窗重读目录：广播里带这个代号。 */
  const packRevRef = useRef(0);
  const [packCount, setPackCount] = useState(0);
  const [importOpen, setImportOpen] = useState(false);

  const applyPetConfig = (patch: Partial<PetConfig>) => {
    const next = normalizePetConfig({ ...petCfg, ...patch });
    setSettings({ aiHubPetSkin: next.skin, aiHubPetSize: next.size, aiHubPetQuiet: next.quiet });
    // 桌宠窗是另一个 webview，它的 store 不会自己刷新：改完广播过去（同主题事件的做法）。
    void emit<PetConfigPatch>(PET_CONFIG_EVENT, { ...next, packRev: packRevRef.current }).catch(() => {});
  };

  /** 好感回到初始值（与 AiHubBubble 的默认一致）；桌宠窗经 storage 事件跟盘上的走。 */
  const resetPetAffinity = () => {
    setSettings({ aiHubPetAffinity: 20 });
  };

  /** 位置回到「首次出现」的默认点：丢掉存档坐标，让 Rust 按主屏右下角重新摆。 */
  const resetPetPosition = () => {
    setSettings({ aiHubBubblePos: undefined });
    void invoke("set_ai_hub_bubble_visible", {
      visible: settings.aiHubBubbleEnabled !== false,
      x: null,
      y: null,
    }).catch(() => {});
  };

  /** 素材包落盘或清空之后：重读、把皮肤切上去（空了就回圆盘），代号 +1 通知桌宠窗。 */
  const afterPackChange = async () => {
    const count = await loadUserPack();
    setPackCount(count);
    packRevRef.current += 1;
    applyPetConfig({ skin: count > 0 ? CUSTOM_SKIN : PANDA_SKIN });
  };

  useEffect(() => {
    void loadUserPack().then(setPackCount);
  }, []);
  const [usageRows, setUsageRows] = useState<AiUsageEntry[]>(() => usageList());
  // 当日桶：跨天自动归零，用来跟「每日预算」比
  const [today, setToday] = useState<AiUsageDay>(() => readAiUsageDay());
  // 逐日历史：走势图用，跨天不归零
  const [trend, setTrend] = useState<AiUsageHistoryPoint[]>(() => readAiUsageHistory());
  // 模型明细口径：默认「近 14 天」与上面走势图同窗口，避免两个数字对不上
  const [usageScope, setUsageScope] = useState<"trend" | "all">("trend");
  useEffect(() => {
    const refresh = () => {
      setUsageRows(usageList());
      setToday(readAiUsageDay());
      setTrend(readAiUsageHistory());
    };
    window.addEventListener("ai-usage-changed", refresh);
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener("ai-usage-changed", refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);
  // 走势图与模型明细共用一套配色：按用量行序取 --chart-*，图上的曲线颜色和列表圆点一致
  const usagePalette = new Map(
    usageRows.slice(0, 12).map((row, i) => [row.modelId, CHART_COLORS[i % CHART_COLORS.length]])
  );
  const trendSeries = (() => {
    const ids = new Set(trend.flatMap((d) => Object.keys(d.byModel)));
    return usageRows
      .filter((r) => ids.has(r.modelId))
      .slice(0, 6)
      .map((r) => ({ id: r.modelId, name: r.modelName, color: usagePalette.get(r.modelId) || CHART_COLORS[0] }));
  })();
  // 近 14 天分模型口径：与走势图同源同窗口，合计恒等于曲线总量；
  // 旧版本记录没有 byModel 的部分归入 other，保证明细加起来能对上
  const trendByModel = (() => {
    const tokens = new Map<string, number>();
    const calls = new Map<string, number>();
    let total = 0;
    for (const d of trend) {
      total += d.tokens;
      for (const [id, v] of Object.entries(d.byModel ?? {})) tokens.set(id, (tokens.get(id) ?? 0) + Number(v));
      for (const [id, v] of Object.entries(d.callsByModel ?? {})) calls.set(id, (calls.get(id) ?? 0) + Number(v));
    }
    const attributed = [...tokens.values()].reduce((s, v) => s + v, 0);
    return { tokens, calls, total, other: Math.max(total - attributed, 0) };
  })();
  const invokeSetAutoStart = useGlobalStore((s) => s.invokeSetAutoStart);
  const invokeGetAutoStart = useGlobalStore((s) => s.invokeGetAutoStart);
  const invokeExportData = useGlobalStore((s) => s.invokeExportData);
  const invokeImportData = useGlobalStore((s) => s.invokeImportData);

  const [appVersion, setAppVersion] = useState("…");
  const [checking, setChecking] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [updateTarget, setUpdateTarget] = useState<{ version: string; htmlUrl: string } | null>(null);
  const [skipped, setSkipped] = useState<string | null>(() => getSkippedVersion());
  const [dataBusy, setDataBusy] = useState(false);
  const [tab, setTab] = useState<SettingsTab>("general");

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 5000);
  }, []);

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion("0.1.0"));
  }, []);

  useEffect(() => {
    invokeGetAutoStart()
      .then((enabled) => {
        if (enabled !== settings.autoStart) {
          setSettings({ autoStart: enabled });
        }
      })
      .catch((e) => {
        showMsg("error", t("general.autoStartReadFailed", { error: String(e) }));
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on mount
  }, []);

  const handleAutoStartChange = async (enabled: boolean) => {
    setSettings({ autoStart: enabled });
    try {
      await invokeSetAutoStart(enabled);
    } catch (e) {
      setSettings({ autoStart: !enabled });
      showMsg("error", t("general.autoStartUpdateFailed", { error: String(e) }));
    }
  };

  const handleThemeChange = (theme: AppTheme) => {
    setSettings({ theme });
    applyDocumentTheme(theme);
    void emit(APP_THEME_CHANGED_EVENT, theme);
  };

  const handleLanguageChange = (lang: "zh-CN" | "en-US") => {
    setSettings({ language: lang });
    i18n.changeLanguage(lang);
  };

  const handleSidebarToggle = () => {
    setSettings({ sidebarCollapsed: !settings.sidebarCollapsed });
  };

  const handleExport = async () => {
    setDataBusy(true);
    try {
      const path = await invokeExportData();
      showMsg("success", t("data.exportOk", { path }));
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("取消")) showMsg("error", t("data.failed", { error: msg }));
    } finally {
      setDataBusy(false);
    }
  };

  const handleImport = async () => {
    const ok = await confirm({
      title: t("data.import"),
      message: `${t("data.importWarn")}\n\n${t("data.confirmImport")}`,
    });
    if (!ok) return;
    setDataBusy(true);
    try {
      const path = await invokeImportData();
      showMsg("success", t("data.importOk", { path }));
      await useGlobalStore.getState().initialize();
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("取消")) showMsg("error", t("data.failed", { error: msg }));
    } finally {
      setDataBusy(false);
    }
  };

  const handleCheckUpdate = async () => {
    setChecking(true);
    setUpdateTarget(null);
    try {
      const local = appVersion === "…" ? await getVersion() : appVersion;
      const result = await checkForUpdates(local);
      if (result.status === "upToDate") {
        showMsg("success", t("about.upToDate", { version: result.remote }));
      } else if (result.status === "updateAvailable") {
        setUpdateTarget({ version: result.remote, htmlUrl: result.htmlUrl });
        // 发现新版走右下角浮框；没接线时（纯浏览器 dev）退回 toast
        if (onNoticeUpdate) {
          onNoticeUpdate({ version: result.remote, htmlUrl: result.htmlUrl });
        } else {
          showMsg(
            "success",
            t("about.updateAvailable", { remote: result.remote, local: result.local })
          );
        }
      } else {
        showMsg("error", t("about.checkFailed", { error: result.message }));
      }
    } catch (e) {
      showMsg("error", t("about.checkFailed", { error: String(e) }));
    } finally {
      setChecking(false);
    }
  };

  const themeLabelKey = (theme: AppTheme) =>
    `general.theme${theme.charAt(0).toUpperCase() + theme.slice(1)}`;

  return (
    <div className="settings-page">
      <nav className="settings-nav" aria-label={t("tabs.aria")}>
        {SETTINGS_TABS.map(({ id, icon: Icon, labelKey }) => (
          <button
            key={id}
            type="button"
            className={`settings-nav-item ${tab === id ? "active" : ""}`}
            aria-current={tab === id ? "page" : undefined}
            onClick={() => setTab(id)}
          >
            <Icon size={14} />
            <span>{t(labelKey)}</span>
          </button>
        ))}
      </nav>

      <div className="settings-cards">
        <div className="card" hidden={tab !== "general"}>
          <div className="card-title">{t("general.title")}</div>
          <div className="card-body">
            <div className="setting-row">
              <span className="setting-label">{t("general.language")}</span>
              <select
                className="input-field"
                style={{ width: 140 }}
                value={settings.language}
                onChange={(e) => handleLanguageChange(e.target.value as "zh-CN" | "en-US")}
              >
                <option value="zh-CN">中文</option>
                <option value="en-US">English</option>
              </select>
            </div>
            <div className="setting-stack">
              <span className="setting-label">{t("general.theme")}</span>
              <div className="theme-selector" role="group" aria-label={t("general.theme")}>
                {THEME_OPTIONS.map((theme) => (
                  <button
                    key={theme}
                    type="button"
                    className={`theme-btn ${settings.theme === theme ? "active" : ""}`}
                    onClick={() => handleThemeChange(theme)}
                  >
                    <span>{t(themeLabelKey(theme))}</span>
                  </button>
                ))}
              </div>
            </div>
            <div className="setting-row">
              <span className="setting-label">{t("general.autoStart")}</span>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={settings.autoStart}
                  onChange={(e) => handleAutoStartChange(e.target.checked)}
                />
                <span className="toggle-slider" />
              </label>
            </div>
            <div className="setting-row">
              <span className="setting-label">{t("general.sidebarCollapsed")}</span>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={settings.sidebarCollapsed}
                  onChange={handleSidebarToggle}
                />
                <span className="toggle-slider" />
              </label>
            </div>
          </div>
        </div>

        {/* 桌面宠物：开关 / 形象 / 素材 / 大小 / 安静模式自成一张卡，跟"天玑怎么干活"分开 */}
        <div className="card" hidden={tab !== "general"}>
          <div className="card-title">{t("aiHub.bubble")}</div>
          <div className="card-body">
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.petEnabled")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.aiHubBubbleEnabled !== false}
                    onChange={(e) => {
                      const on = e.target.checked;
                      const pos = settings.aiHubBubblePos;
                      // Legacy HiDPI bug saved top-band coords; let Rust use default instead.
                      const unsafe = pos && Number.isFinite(pos.y) && pos.y < 120;
                      setSettings(
                        unsafe
                          ? { aiHubBubbleEnabled: on, aiHubBubblePos: undefined }
                          : { aiHubBubbleEnabled: on }
                      );
                      void invoke("set_ai_hub_bubble_visible", {
                        visible: on,
                        x: unsafe ? null : (pos?.x ?? null),
                        y: unsafe ? null : (pos?.y ?? null),
                      }).catch(() => {});
                    }}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.bubbleHint")}</p>
            </div>

            <div className="setting-item">
              <div className="setting-row pet-choice-row">
                <span className="setting-label">{t("aiHub.petSkin")}</span>
                <div className="pet-chips">
                  {PET_ORDER.filter((id) => id !== CUSTOM_SKIN || packCount > 0).map((id) => (
                    <button
                      key={id}
                      type="button"
                      className={`pet-chip${petCfg.skin === id ? " is-active" : ""}`}
                      onClick={() => applyPetConfig({ skin: id })}
                    >
                      {t(`aiHub.petName${id.charAt(0).toUpperCase()}${id.slice(1)}`)}
                    </button>
                  ))}
                </div>
              </div>
            </div>

            <div className="setting-item">
              <div className="setting-row pet-choice-row">
                <span className="setting-label">{t("aiHub.petSize")}</span>
                <div className="pet-chips">
                  {PET_SIZES.map((px) => (
                    <button
                      key={px}
                      type="button"
                      className={`pet-chip${petCfg.size === px ? " is-active" : ""}`}
                      onClick={() => applyPetConfig({ size: px })}
                    >
                      {px}px
                    </button>
                  ))}
                </div>
              </div>
              <p className="setting-hint">{t("aiHub.petSizeHint")}</p>
            </div>

            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.petQuiet")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={petCfg.quiet}
                    onChange={(e) => applyPetConfig({ quiet: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.petQuietHint")}</p>
            </div>

            <div className="setting-item">
              <div className="setting-row pet-choice-row">
                <span className="setting-label">{t("aiHub.petReset")}</span>
                <div className="pet-chips">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={resetPetAffinity}
                  >
                    {t("aiHub.petResetAffinity")}
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={resetPetPosition}
                  >
                    {t("aiHub.petResetPos")}
                  </button>
                </div>
              </div>
              <p className="setting-hint">{t("aiHub.petResetHint")}</p>
            </div>

            <div className="setting-item">
              <div className="setting-row pet-choice-row">
                <span className="setting-label">{t("aiHub.petImport")}</span>
                <div className="pet-chips">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => setImportOpen(true)}
                  >
                    {t("aiHub.petImportPick")}
                  </button>
                  {packCount > 0 && (
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() =>
                        void (async () => {
                          await invoke("clear_pet_pack");
                          await afterPackChange();
                        })()
                      }
                    >
                      {t("aiHub.petImportClear")}
                    </button>
                  )}
                </div>
              </div>
              <p className="setting-hint">
                {packCount > 0
                  ? t("aiHub.petImportReady", { n: packCount })
                  : t("aiHub.petImportEmpty")}
              </p>
            </div>
          </div>
        </div>

        {/* 数据管理（导出 / 导入）：挂在通用设置末尾 */}
        <div className="card" hidden={tab !== "general"}>
          <div className="card-title">{t("data.title")}</div>
          <div className="card-body">
            <p className="setting-hint">{t("data.exportWarn")}</p>
            <p className="setting-hint">{t("data.importWarn")}</p>
            <div className="setting-inline-controls">
              <button type="button" className="btn btn-secondary" disabled={dataBusy} onClick={() => void handleExport()}>
                {t("data.export")}
              </button>
              <button type="button" className="btn btn-secondary" disabled={dataBusy} onClick={() => void handleImport()}>
                {t("data.import")}
              </button>
            </div>
          </div>
        </div>

        {/* 菜单配置：拖拽排序 + 改名，组件自带工具条与分组卡，不再套一层 card */}
        <div className="settings-pane" hidden={tab !== "menu"}>
          <MenuManager />
        </div>

        <div className="card" hidden={tab !== "data"}>
          <div className="card-title">{t("usage.title")}</div>
          <div className="card-body">
            <div className="usage-stat">
              <div className="usage-stat-main">
                <span className="usage-stat-label">
                  {t("usage.statToday")}
                  <span className="usage-stat-calls">{t("usage.statCalls", { calls: today.calls })}</span>
                </span>
                <span
                  className={`usage-stat-value${
                    (settings.aiUsageDailyBudget ?? 0) > 0 && today.tokens > (settings.aiUsageDailyBudget ?? 0)
                      ? " is-over"
                      : ""
                  }`}
                >
                  {today.tokens.toLocaleString()}
                  <small> tokens</small>
                </span>
              </div>
              <label className="usage-stat-budget">
                <span className="setting-label">{t("usage.dailyBudget")}</span>
                <select
                  className="input-field"
                  style={{ width: 110 }}
                  value={String(settings.aiUsageDailyBudget ?? 0)}
                  onChange={(e) => setSettings({ aiUsageDailyBudget: Number(e.target.value) })}
                >
                  <option value="0">{t("usage.budgetOff")}</option>
                  <option value="50000">50k</option>
                  <option value="200000">200k</option>
                  <option value="1000000">1M</option>
                </select>
              </label>
            </div>
            {(settings.aiUsageDailyBudget ?? 0) > 0 && (
              <div
                className={`usage-budget-track${
                  today.tokens > (settings.aiUsageDailyBudget ?? 0) ? " is-over" : ""
                }`}
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={settings.aiUsageDailyBudget ?? 0}
                aria-valuenow={today.tokens}
              >
                <i
                  style={{
                    width: `${Math.min(
                      100,
                      Math.round((today.tokens / (settings.aiUsageDailyBudget || 1)) * 100)
                    )}%`,
                  }}
                />
              </div>
            )}
            {(settings.aiUsageDailyBudget ?? 0) > 0 && (
              <p className="setting-hint">
                {t(
                  today.tokens > (settings.aiUsageDailyBudget ?? 0) ? "usage.todayOver" : "usage.todayBudget",
                  {
                    used: today.tokens.toLocaleString(),
                    cap: (settings.aiUsageDailyBudget ?? 0).toLocaleString(),
                    calls: today.calls,
                    pct: Math.round((today.tokens / (settings.aiUsageDailyBudget || 1)) * 100),
                  }
                )}
              </p>
            )}
            <p className="setting-hint">{t("usage.hint")}</p>
            <UsageTrend days={trend} budget={settings.aiUsageDailyBudget ?? 0} series={trendSeries} />
            <div className="usage-models-head">
              <span className="usage-models-title">{t("usage.modelsTitle")}</span>
              <span className="usage-trend-switch" role="tablist" aria-label={t("usage.modelsTitle")}>
                <button
                  type="button"
                  role="tab"
                  aria-selected={usageScope === "trend"}
                  className={`usage-trend-switch-btn${usageScope === "trend" ? " is-active" : ""}`}
                  onClick={() => setUsageScope("trend")}
                >
                  {t("usage.scopeTrend")}
                </button>
                <button
                  type="button"
                  role="tab"
                  aria-selected={usageScope === "all"}
                  className={`usage-trend-switch-btn${usageScope === "all" ? " is-active" : ""}`}
                  onClick={() => setUsageScope("all")}
                >
                  {t("usage.scopeAll")}
                </button>
              </span>
            </div>
            {(() => {
              const allTokensOf = (r: AiUsageEntry) =>
                r.promptTokens + r.completionTokens > 0 ? r.promptTokens + r.completionTokens : r.estimatedTokens;
              const rows = (
                usageScope === "all"
                  ? usageRows.map((r) => ({
                      id: r.modelId,
                      name: r.modelName,
                      calls: r.calls,
                      tokens: allTokensOf(r),
                      color: usagePalette.get(r.modelId) || "var(--text-3)",
                    }))
                  : usageRows
                      .filter((r) => (trendByModel.tokens.get(r.modelId) ?? 0) > 0)
                      .map((r) => ({
                        id: r.modelId,
                        name: r.modelName,
                        calls: trendByModel.calls.get(r.modelId) ?? 0,
                        tokens: trendByModel.tokens.get(r.modelId) ?? 0,
                        color: usagePalette.get(r.modelId) || "var(--text-3)",
                      }))
                      .sort((a, b) => b.tokens - a.tokens)
              ).slice(0, 12);
              const total =
                usageScope === "all" ? rows.reduce((s, r) => s + r.tokens, 0) : trendByModel.total;
              const showOther = usageScope === "trend" && trendByModel.other > 0;
              const isEmpty = usageScope === "all" ? usageRows.length === 0 : trendByModel.total === 0;
              if (isEmpty) {
                return <p className="setting-hint">{usageScope === "trend" ? t("usage.emptyTrend") : t("usage.empty")}</p>;
              }
              const rowOf = (id: string, name: string, calls: number, tokens: number, color: string) => {
                const share = total > 0 ? Math.round((tokens / total) * 100) : 0;
                // 「OR · openrouter/free」这类名字拆成主名 + 弱色后缀，避免和左侧色点重复出现两个「·」
                const sep = name.indexOf(" · ");
                const [nameMain, nameSub] = sep > 0 ? [name.slice(0, sep), name.slice(sep + 3)] : [name, ""];
                return (
                  <li key={id} className="usage-model">
                    <span className="usage-model-dot" style={{ background: color }} />
                    <div className="usage-model-main">
                      <div className="usage-model-head">
                        <strong>
                          {nameMain}
                          {nameSub && <span className="usage-model-sub"> · {nameSub}</span>}
                        </strong>
                        <span className="usage-model-nums">
                          <span className="usage-model-tokens">~{tokens.toLocaleString()} tok</span>
                          <span className="usage-model-share">{share}%</span>
                          {calls > 0 && (
                            <span className="runtime-muted">{t("usage.statCalls", { calls })}</span>
                          )}
                        </span>
                      </div>
                      <div className="usage-model-track">
                        <i style={{ width: `${Math.max(share, 2)}%`, background: color }} />
                      </div>
                    </div>
                  </li>
                );
              };
              return (
                <ul className="usage-models">
                  {rows.map((r) => rowOf(r.id, r.name, r.calls, r.tokens, r.color))}
                  {showOther && rowOf("other", t("usage.otherLegacy"), 0, trendByModel.other, "var(--text-3)")}
                </ul>
              );
            })()}
            <div className="setting-inline-controls">
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={usageRows.length === 0}
                onClick={() => {
                  clearAiUsage();
                  setUsageRows([]);
                }}
              >
                {t("usage.clear")}
              </button>
            </div>
          </div>
        </div>

        {/* 关于程序：独立分类 */}
        <div className="card" hidden={tab !== "about"}>
          <div className="card-title">{t("about.title")}</div>
          <div className="card-body about-card-body">
            <div className="setting-row">
              <span className="setting-label">{t("about.version")}</span>
              <span className="setting-value about-version">
                {appVersion}
                {updateTarget && <span className="about-version-next">→ {updateTarget.version}</span>}
              </span>
            </div>
            <div className="setting-row">
              <span className="setting-label">{t("about.license")}</span>
              <span className="setting-value">MIT</span>
            </div>
            <p className="setting-hint">{t("about.privacyNote")}</p>
            <div className="setting-inline-controls">
              <button
                className="btn btn-secondary btn-small"
                disabled={checking}
                onClick={handleCheckUpdate}
              >
                {checking ? (
                  <>
                    <Loader2 size={12} className="spin" /> {t("about.checking")}
                  </>
                ) : (
                  t("about.checkUpdate")
                )}
              </button>
              <button
                className="btn btn-secondary btn-small"
                onClick={() => {
                  const url = updateTarget?.htmlUrl || APP_RELEASES_URL;
                  // window.open is silently swallowed in the Tauri webview — open the
                  // URL in the system browser via our http(s)-validated command instead.
                  invoke("open_in_browser", { url }).catch(() => window.open(url, "_blank"));
                }}
              >
                {t("about.github")}
              </button>
              {skipped && (
                <button
                  className="btn btn-secondary btn-small"
                  onClick={() => {
                    clearSkippedVersion();
                    setSkipped(null);
                    showMsg("success", t("about.skippedCleared", { version: skipped }));
                  }}
                  title={t("about.clearSkipHint", { version: skipped })}
                >
                  {t("about.clearSkip", { version: skipped })}
                </button>
              )}
            </div>
          </div>
        </div>

      </div>

      {importOpen && (
        <PetPackImport
          onClose={() => setImportOpen(false)}
          onApplied={() => void afterPackChange().then(() => setImportOpen(false))}
        />
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          <span className="toast-text">{message.text}</span>
        </div>
      )}
    </div>
  );
}

export default Settings;
