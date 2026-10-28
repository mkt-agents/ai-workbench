import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff } from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import { useGlobalStore } from "../core/store";
import type { AppSettings, DshModelCatalog } from "../core/types";
import {
  applyWecomChannel,
  protectSecret,
  revealWecomSecret,
  approveWecomPairing,
  fetchDshModels,
  fetchWecomPairings,
  fetchWecomStatus,
  restartWecomChannel,
  revokeWecomUser,
  syncDshProfile,
  type WecomPairing,
  type WecomStatus,
} from "../lib/wecomChannel";

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

/** 一行：左边字段名，右边控件（宽度统一，说明另起一行铺满）。 */
function Row({
  label,
  control,
  hint,
}: {
  label: string;
  control: React.ReactNode;
  hint?: React.ReactNode;
}) {
  return (
    <div className="wecom-row">
      <span className="wecom-row-label">{label}</span>
      <div className="wecom-row-control">{control}</div>
      {hint ? <div className="wecom-row-hint">{hint}</div> : null}
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
  // DSH 引擎的模型目录（ACP configOptions），按引擎就绪懒加载
  const [catalog, setCatalog] = useState<DshModelCatalog | null>(null);
  const [catalogBusy, setCatalogBusy] = useState(false);
  const [catalogError, setCatalogError] = useState("");

  const botId = useFieldDraft(settings.wecomBotId ?? "");
  const corpId = useFieldDraft(settings.wecomCorpId ?? "");
  const displayName = useFieldDraft(settings.wecomBotDisplayName ?? "");
  const allowUsers = useFieldDraft((settings.wecomAllowUsers ?? []).join(", "));
  const allowGroups = useFieldDraft((settings.wecomAllowGroups ?? []).join(", "));
  const repoPath = useFieldDraft(settings.wecomRepoPath ?? "");

  const refresh = useCallback(async () => {
    try {
      const [st, pairs] = await Promise.all([fetchWecomStatus(), fetchWecomPairings()]);
      setStatus(st);
      setPairings(pairs);
    } catch (e) {
      setMsg(String(e));
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

  const modelValue = settings.wecomDshModel ?? "";
  const modelKnown =
    !modelValue ||
    Boolean(catalog?.groups.some((g) => g.options.some((o) => o.value === modelValue)));
  const modelCount =
    catalog?.groups.reduce((total, group) => total + group.options.length, 0) ?? 0;

  return (
    <div className="wecom-settings">
      <p className="setting-hint wecom-lead">{t("wecom.hint")}</p>

      {/* 1 · 通道：连不连得上（开关放在块标题右侧，省一行） */}
      <div className="net-panel">
        <div className="wecom-panel-head">
          <div className="hub-group-title">{t("wecom.groupChannel")}</div>
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
        <div className="wecom-rows">
          <Row
            label={t("wecom.status")}
            control={
              <>
                <span
                  className={`wecom-status-pill ${statusClass(status?.state)}`}
                  title={status?.detail || undefined}
                >
                  <i className="wecom-status-dot" aria-hidden />
                  {stateLabel}
                </span>
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
              </>
            }
            hint={status?.detail ? <p className="setting-hint">{status.detail}</p> : undefined}
          />

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
        </div>
      </div>

      {/* 2 · AI 引擎：谁来回话（DSH 档把状态、镜像、模型都收在这一块） */}
      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupEngine")}</div>
        <div className="wecom-rows">
          <Row
            label={t("wecom.engine")}
            control={
              <select
                className="input-field"
                value={engine}
                disabled={busy}
                onChange={(e) => void push({ wecomEngine: e.target.value })}
              >
                <option value="tianji">{t("wecom.engineTianji")}</option>
                <option value="dsh">{t("wecom.engineDsh")}</option>
              </select>
            }
            hint={<Hint summary={t("wecom.engineHintShort")} details={t("wecom.engineHint")} />}
          />

          {engine === "dsh" ? (
            <Row
              label={t("wecom.dshState")}
              control={
                <>
                  <span
                    className={`wecom-status-pill ${dshStateClass(dshState)}`}
                    title={status?.dsh?.detail || undefined}
                  >
                    <i className="wecom-status-dot" aria-hidden />
                    {dshLabel}
                  </span>
                  {status?.dsh?.version ? (
                    <span className="setting-hint">v{status.dsh.version}</span>
                  ) : null}
                </>
              }
              hint={
                status?.dsh?.detail ? <p className="setting-hint">{status.dsh.detail}</p> : undefined
              }
            />
          ) : null}

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
                  <span className="setting-hint wecom-row-meta">
                    {mirror
                      ? t("wecom.dshMirrorLine", {
                          source: mirror.source,
                          target: mirror.target,
                          time: new Date(mirror.syncedAt).toLocaleString(),
                        })
                      : t("wecom.dshMirrorNone")}
                  </span>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => void resyncProfile()}
                  >
                    {t("wecom.dshResync")}
                  </button>
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
              label={t("wecom.dshModelRow")}
              control={
                <>
                  <select
                    className="input-field wecom-split"
                    value={modelValue}
                    disabled={busy || catalogBusy}
                    onChange={(e) => void push({ wecomDshModel: e.target.value })}
                  >
                    <option value="">{t("wecom.dshModelDefault")}</option>
                    {!modelKnown ? (
                      <option value={modelValue}>{t("wecom.dshModelMissing")}</option>
                    ) : null}
                    {(catalog?.groups ?? []).map((group) => (
                      <optgroup key={group.group || group.name} label={group.name || group.group}>
                        {group.options.map((option) => (
                          <option key={option.value} value={option.value}>
                            {option.name || option.value}
                          </option>
                        ))}
                      </optgroup>
                    ))}
                  </select>
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
                </>
              }
              hint={
                <div className="wecom-inline">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy || catalogBusy || !dshReady}
                    onClick={() => void loadModels(true)}
                  >
                    {catalogBusy ? t("wecom.dshLoading") : t("wecom.dshRefreshModels")}
                  </button>
                  <span className="setting-hint">
                    {catalogError
                      ? catalogError
                      : !dshReady
                        ? t("wecom.dshModelsOff")
                        : catalog && modelCount === 0
                          ? t("wecom.dshModelsEmpty")
                          : t("wecom.dshModelsHint", { count: modelCount })}
                  </span>
                </div>
              }
            />
          ) : null}
        </div>
      </div>

      {/* 3 · 凭据：长连接必填，业务 API 只有文档/日程/表格要 */}
      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupCredentials")}</div>
        <div className="wecom-rows">
          <div className="wecom-subhead">{t("wecom.subBot")}</div>

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

      {/* 4 · 访问策略：谁能用 + 能用什么 */}
      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupAccess")}</div>
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

      {/* 5 · 配对：私聊配对码与已批准用户 */}
      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupPairing")}</div>
        <div className="setting-item">
          <span className="setting-label">{t("wecom.pairingTitle")}</span>
          {pairings.length === 0 ? (
            <p className="setting-hint">{t("wecom.pairingEmpty")}</p>
          ) : (
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

      {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
    </div>
  );
}
