import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff } from "lucide-react";
import { useConfirm } from "./ConfirmModal";
import { useGlobalStore } from "../core/store";
import type { AppSettings } from "../core/types";
import {
  applyWecomChannel,
  protectSecret,
  revealWecomSecret,
  approveWecomPairing,
  fetchWecomPairings,
  fetchWecomStatus,
  restartWecomChannel,
  revokeWecomUser,
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

  return (
    <div className="wecom-settings">
      <p className="setting-hint wecom-lead">{t("wecom.hint")}</p>

      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupConnection")}</div>
        <div className="net-grid">
          <div className="net-field">
            <span className="setting-label">{t("wecom.enabled")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={Boolean(settings.wecomEnabled)}
                disabled={busy}
                onChange={(e) => void push({ wecomEnabled: e.target.checked })}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.status")}</span>
            <div className="wecom-status">
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
            </div>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.botId")}</span>
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
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.botSecret")}</span>
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
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.corpId")}</span>
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
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.corpSecret")}</span>
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
          </div>

          <div className="net-field net-span-2">
            <p className="setting-hint">{t("wecom.corpHint")}</p>
          </div>

          <div className="net-field net-span-2">
            <span className="setting-label">{t("wecom.displayName")}</span>
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
          </div>
        </div>
      </div>

      <div className="net-panel">
        <div className="hub-group-title">{t("wecom.groupAccess")}</div>
        <div className="net-grid">
          <div className="net-field">
            <span className="setting-label">{t("wecom.dmPolicy")}</span>
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
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.allowUsers")}</span>
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
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.groupPolicy")}</span>
            <select
              className="input-field"
              value={settings.wecomGroupPolicy ?? "allowlist"}
              onChange={(e) => void push({ wecomGroupPolicy: e.target.value })}
            >
              <option value="allowlist">{t("wecom.policy.allowlist")}</option>
              <option value="open">{t("wecom.policy.open")}</option>
              <option value="disabled">{t("wecom.policy.disabled")}</option>
            </select>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.allowGroups")}</span>
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
          </div>

          <div className="setting-item">
            <div className="net-field">
              <span className="setting-label">{t("wecom.requireMention")}</span>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={settings.wecomRequireMention !== false}
                  onChange={(e) => void push({ wecomRequireMention: e.target.checked })}
                />
                <span className="toggle-slider" />
              </label>
            </div>
            <p className="setting-hint">{t("wecom.requireMentionHint")}</p>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.permission")}</span>
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
          </div>

          <div className="net-field net-span-2">
            <span className="setting-label">{t("wecom.repoPath")}</span>
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
          </div>
        </div>
      </div>

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
