import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { AppSettings } from "../core/types";
import type { HarnessConfigView } from "../core/store/invocations";
import { HARNESS_MODES, type HarnessMode } from "./aiHub/harness";
import ProfileModal from "./aiHub/ProfileModal";
import { mergeProfiles } from "../lib/aiHubProfiles";
import { registerAiHubShortcut, DEFAULT_SHORTCUT } from "../lib/aiHubShortcut";
import NetExtensionPanel from "./NetExtensionPanel";

/** 一条「以后都允许」：记在哪一层 + 规则本身。 */
type RememberedAllow = {
  scope: "global" | "repo" | "branch";
  /** 仓库根路径；`global` 时为空。 */
  repo: string;
  /** 分支名；只有 `branch` 层才有。 */
  branch: string;
  kind: string;
};

/** 三层摊平成一张表，列表展示与逐条删除共用。 */
function rememberedAllowEntries(s: AppSettings): RememberedAllow[] {
  const out: RememberedAllow[] = (s.agentAlwaysAllow || []).map((kind) => ({
    scope: "global",
    repo: "",
    branch: "",
    kind,
  }));
  for (const [repo, list] of Object.entries(s.agentAlwaysAllowByRepo || {})) {
    for (const kind of list || []) out.push({ scope: "repo", repo, branch: "", kind });
  }
  for (const [repo, perRepo] of Object.entries(s.agentAlwaysAllowByBranch || {})) {
    for (const [branch, list] of Object.entries(perRepo || {})) {
      for (const kind of list || []) out.push({ scope: "branch", repo, branch, kind });
    }
  }
  return out;
}

/** 「以后都允许」全局 + 各仓库 + 各分支的总条数，清空按钮和文案共用。 */
function rememberedAllowCount(s: AppSettings): number {
  return rememberedAllowEntries(s).length;
}

/** 列表里只露最后一段目录名，完整路径放 title。 */
function shortRepoName(repo: string): string {
  const parts = repo.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] || repo;
}

/** 摘掉一条；空掉的仓库 / 分支整个删掉，别在设置里留空对象。 */
function withoutRemembered(s: AppSettings, entry: RememberedAllow): Partial<AppSettings> {
  if (entry.scope === "global") {
    return { agentAlwaysAllow: (s.agentAlwaysAllow || []).filter((k) => k !== entry.kind) };
  }
  if (entry.scope === "repo") {
    const byRepo = { ...(s.agentAlwaysAllowByRepo || {}) };
    const list = (byRepo[entry.repo] || []).filter((k) => k !== entry.kind);
    if (list.length) byRepo[entry.repo] = list;
    else delete byRepo[entry.repo];
    return { agentAlwaysAllowByRepo: byRepo };
  }
  const byBranch = { ...(s.agentAlwaysAllowByBranch || {}) };
  const perRepo = { ...(byBranch[entry.repo] || {}) };
  const list = (perRepo[entry.branch] || []).filter((k) => k !== entry.kind);
  if (list.length) perRepo[entry.branch] = list;
  else delete perRepo[entry.branch];
  if (Object.keys(perRepo).length) byBranch[entry.repo] = perRepo;
  else delete byBranch[entry.repo];
  return { agentAlwaysAllowByBranch: byBranch };
}

/**
 * 智能互联 → 天玑助手。
 *
 * 从设置页整块搬出来的两块内容，页内用一级 Tab 切：
 * - 交互与权限：窗口行为 / 每轮怎么答 / 写入与审批 / 能力开关 / 上下文预算
 * - 联网扩展：网页抓取 / 联网搜索 / MCP 服务 / 天玑 OpenAI 兼容端点
 */
export default function TianjiAssistantPage() {
  const { t } = useTranslation("settings");
  /** 角色档位的名字与说明在天玑的命名空间里（内置档位共用同一套文案）。 */
  const { t: tHub } = useTranslation("aihub");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const invokeHarnessConfig = useGlobalStore((s) => s.invokeHarnessConfig);

  /** 页内一级 Tab：交互与权限 / 联网扩展 */
  const [section, setSection] = useState<"perm" | "net">("perm");
  const [hubGroup, setHubGroup] = useState<"interaction" | "capability" | "budget">("interaction");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [profileOpen, setProfileOpen] = useState(false);
  const profiles = useMemo(() => mergeProfiles(settings.aiHubProfiles), [settings.aiHubProfiles]);
  /** 钉住的任务模式：undefined / "code" 都算自动，与天玑侧同一套判定。 */
  const pinnedTaskMode =
    settings.aiHubTaskMode &&
    settings.aiHubTaskMode !== "code" &&
    HARNESS_MODES.includes(settings.aiHubTaskMode as HarnessMode)
      ? settings.aiHubTaskMode
      : "";

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 5000);
  }, []);

  // 项目 harness 配置（`.ai-workbench/` 与全局目录）：只读展示来源、技能与 hooks 数量
  const [harnessConfig, setHarnessConfig] = useState<HarnessConfigView | null>(null);
  const harnessRoot = settings.currentGitRepo?.trim() || "";
  useEffect(() => {
    if (!harnessRoot) {
      setHarnessConfig(null);
      return;
    }
    let alive = true;
    void invokeHarnessConfig(harnessRoot)
      .then((config) => {
        if (alive) setHarnessConfig(config);
      })
      .catch(() => {
        if (alive) setHarnessConfig(null);
      });
    return () => {
      alive = false;
    };
  }, [harnessRoot, invokeHarnessConfig]);

  // 快捷键草稿：设置晚于挂载才水合时也要跟上已保存的值
  const [shortcutDraft, setShortcutDraft] = useState(settings.aiHubShortcut || DEFAULT_SHORTCUT);
  useEffect(() => {
    setShortcutDraft(settings.aiHubShortcut || DEFAULT_SHORTCUT);
  }, [settings.aiHubShortcut]);

  const handleApplyShortcut = async () => {
    const result = await registerAiHubShortcut(shortcutDraft);
    if (result.ok) {
      setSettings({ aiHubShortcut: shortcutDraft.trim() || DEFAULT_SHORTCUT });
      showMsg("success", t("aiHub.shortcutOk", { shortcut: shortcutDraft }));
    } else {
      showMsg("error", t("aiHub.shortcutFail", { error: result.error || "unknown" }));
    }
  };

  return (
    <div className="settings-page is-solo">
      <div className="card">
        {/* 一级 Tab 直接占卡片标题位：页面大标题已经写着「天玑助手」，不再重复一遍 */}
        <div className="card-title is-tabs">
          <div className="hub-tabs is-channel" role="tablist" aria-label={t("aiHub.title")}>
            {(
              [
                ["perm", "tabs.groupPerm"],
                ["net", "tabs.groupNetExt"],
              ] as const
            ).map(([id, labelKey]) => (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={section === id}
                className={`hub-tab${section === id ? " is-active" : ""}`}
                onClick={() => setSection(id)}
              >
                {t(labelKey)}
              </button>
            ))}
          </div>
        </div>
        <div className="card-body">
          {/* 两块都常驻，切 Tab 只切显隐：MCP 草稿、白名单草稿不会因为切走就丢 */}
          <div hidden={section !== "perm"}>
          <div className="hub-tabs" role="tablist" aria-label={t("aiHub.title")}>
            {(
              [
                ["interaction", "tabs.groupInteraction"],
                ["capability", "tabs.groupCapability"],
                ["budget", "tabs.groupBudget"],
              ] as const
            ).map(([id, labelKey]) => (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={hubGroup === id}
                className={`hub-tab${hubGroup === id ? " is-active" : ""}`}
                onClick={() => setHubGroup(id)}
              >
                {t(labelKey)}
              </button>
            ))}
          </div>

          <section className="hub-group" hidden={hubGroup !== "interaction"}>
            {/* 按「影响面」分三段：窗口 → 每轮怎么答 → 写入与审批（安全类压轴） */}
            <div className="hub-group-title">{t("aiHub.groupWindow")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.shortcut")}</span>
                <div className="setting-row-controls">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => void handleApplyShortcut()}
                  >
                    {t("aiHub.shortcutApply")}
                  </button>
                  <input
                    className="input-field setting-shortcut-input"
                    value={shortcutDraft}
                    onChange={(e) => setShortcutDraft(e.target.value)}
                  />
                </div>
              </div>
              <p className="setting-hint">{t("aiHub.shortcutHint")}</p>
            </div>
            <div className="hub-group-title">{t("aiHub.groupAnswer")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.taskMode")}</span>
                <select
                  className="input-field"
                  style={{ width: 160 }}
                  value={pinnedTaskMode ?? ""}
                  onChange={(e) => setSettings({ aiHubTaskMode: e.target.value || undefined })}
                >
                  <option value="">{t("aiHub.taskModeAuto")}</option>
                  {HARNESS_MODES.filter((option) => option !== "code").map((option) => (
                    <option key={option} value={option}>
                      {t(`aiHub.modeNames.${option}`)}
                    </option>
                  ))}
                </select>
              </div>
              <p className="setting-hint">{t("aiHub.taskModeHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.profile")}</span>
                <div className="setting-row-controls">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => setProfileOpen(true)}
                  >
                    {t("aiHub.profileManage")}
                  </button>
                  <select
                    className="input-field"
                    style={{ width: 160 }}
                    value={settings.aiHubProfileId ?? ""}
                    onChange={(e) => setSettings({ aiHubProfileId: e.target.value || undefined })}
                  >
                    <option value="">{t("aiHub.profileNone")}</option>
                    {profiles.map((p) => (
                      <option key={p.id} value={p.id}>
                        {tHub(p.nameKey || "", { defaultValue: p.name })}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
              <p className="setting-hint">{t("aiHub.profileHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.planFirst")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentPlanFirst === true}
                    onChange={(e) => setSettings({ agentPlanFirst: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.planFirstHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.suggest")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.aiHubSuggest !== false}
                    onChange={(e) => setSettings({ aiHubSuggest: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.suggestHint")}</p>
            </div>
            <div className="hub-group-title">{t("aiHub.groupWrite")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.diffConfirm")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentDiffConfirm === true}
                    onChange={(e) => setSettings({ agentDiffConfirm: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.diffConfirmHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.remembered")}</span>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={rememberedAllowCount(settings) === 0}
                  onClick={() =>
                    setSettings({
                      agentAlwaysAllow: [],
                      agentAlwaysAllowByRepo: {},
                      agentAlwaysAllowByBranch: {},
                    })
                  }
                >
                  {t("aiHub.rememberedClear")}
                </button>
              </div>
              <p className="setting-hint">
                {rememberedAllowCount(settings) > 0
                  ? t("aiHub.rememberedCount", { n: rememberedAllowCount(settings) })
                  : t("aiHub.rememberedEmpty")}
              </p>
              <p className="setting-hint">{t("aiHub.rememberedScope")}</p>
              {rememberedAllowCount(settings) > 0 && (
                <details className="remembered-list">
                  <summary>{t("aiHub.rememberedListTitle", { n: rememberedAllowCount(settings) })}</summary>
                  {rememberedAllowEntries(settings).map((entry) => (
                    <div
                      className="remembered-row"
                      key={`${entry.scope}|${entry.repo}|${entry.branch}|${entry.kind}`}
                    >
                      <span className="remembered-scope" title={entry.repo || undefined}>
                        {entry.scope === "global"
                          ? t("aiHub.rememberedScopeGlobal")
                          : entry.scope === "repo"
                            ? t("aiHub.rememberedScopeRepo", { repo: shortRepoName(entry.repo) })
                            : t("aiHub.rememberedScopeBranch", {
                                repo: shortRepoName(entry.repo),
                                branch: entry.branch,
                              })}
                      </span>
                      <code className="remembered-kind">{entry.kind}</code>
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => setSettings(withoutRemembered(settings, entry))}
                      >
                        {t("aiHub.rememberedRemove")}
                      </button>
                    </div>
                  ))}
                </details>
              )}
            </div>
          </section>

          <section className="hub-group" hidden={hubGroup !== "capability"}>
            <div className="hub-group-title">{t("aiHub.groupRead")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.parallelReads")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentParallelReads !== false}
                    onChange={(e) => setSettings({ agentParallelReads: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.parallelReadsHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.grepIgnore")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentGrepIgnore !== false}
                    onChange={(e) => setSettings({ agentGrepIgnore: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.grepIgnoreHint")}</p>
            </div>
            <div className="hub-group-title">{t("aiHub.groupRun")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.retries")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentRetries !== false}
                    onChange={(e) => setSettings({ agentRetries: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.retriesHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.subagents")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentSubagents === true}
                    onChange={(e) => setSettings({ agentSubagents: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.subagentsHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.subagentWrite")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentSubagentWrite === true}
                    disabled={settings.agentSubagents !== true}
                    onChange={(e) => setSettings({ agentSubagentWrite: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.subagentWriteHint")}</p>
            </div>
            <div className="hub-group-title">{t("aiHub.groupProject")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.harnessConfig")}</span>
                <span className="setting-value">
                  {harnessConfig
                    ? t("aiHub.harnessCounts", {
                        skills: harnessConfig.skills.length,
                        hooks: harnessConfig.hooks.length,
                      })
                    : "—"}
                </span>
              </div>
              <p className="setting-hint">
                {harnessConfig
                  ? t("aiHub.harnessSources", {
                      project: harnessConfig.projectDir || "—",
                      global: harnessConfig.globalDir || "—",
                    })
                  : t("aiHub.harnessHint")}
              </p>
            </div>
          </section>

          <section className="hub-group" hidden={hubGroup !== "budget"}>
            <div className="hub-group-title">{t("aiHub.groupContext")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.contextTokens")}</span>
                <select
                  className="input-field"
                  style={{ width: 120 }}
                  value={String(settings.agentContextTokens ?? 48000)}
                  onChange={(e) => setSettings({ agentContextTokens: Number(e.target.value) })}
                >
                  <option value="24000">24k</option>
                  <option value="48000">48k</option>
                  <option value="96000">96k</option>
                </select>
              </div>
              <p className="setting-hint">{t("aiHub.contextTokensHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.resultCap")}</span>
                <select
                  className="input-field"
                  style={{ width: 120 }}
                  value={String(settings.agentResultCap ?? 12000)}
                  onChange={(e) => setSettings({ agentResultCap: Number(e.target.value) })}
                >
                  <option value="6000">6k</option>
                  <option value="12000">12k</option>
                  <option value="24000">24k</option>
                </select>
              </div>
              <p className="setting-hint">{t("aiHub.resultCapHint")}</p>
            </div>
            <div className="hub-group-title">{t("aiHub.groupCompat")}</div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.anthropicStream")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentAnthropicStream !== false}
                    onChange={(e) => setSettings({ agentAnthropicStream: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.anthropicStreamHint")}</p>
            </div>
            <div className="setting-item">
              <div className="setting-row">
                <span className="setting-label">{t("aiHub.freshContext")}</span>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={settings.agentFreshContext !== false}
                    onChange={(e) => setSettings({ agentFreshContext: e.target.checked })}
                  />
                  <span className="toggle-slider" />
                </label>
              </div>
              <p className="setting-hint">{t("aiHub.freshContextHint")}</p>
            </div>
          </section>
          </div>

          <div hidden={section !== "net"}>
            <NetExtensionPanel />
          </div>
        </div>
      </div>

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          <span className="toast-text">{message.text}</span>
        </div>
      )}

      {profileOpen && (
        <ProfileModal
          custom={settings.aiHubProfiles || []}
          onChange={(next) => setSettings({ aiHubProfiles: next })}
          onClose={() => setProfileOpen(false)}
        />
      )}
    </div>
  );
}
