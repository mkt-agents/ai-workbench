import { useEffect, useState, type MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ExternalLink, Eye, EyeOff, Plus, RefreshCw } from "lucide-react";
import {
  detectInstalled,
  QUOTA_PROVIDERS,
  type InstallState,
  type QuotaCardId,
  type QuotaProvider,
} from "../lib/quotaProviders";
import type { CardStatus } from "../lib/quotaCache";

/** 右侧栏里的一个已连接账号；`hidden` = 卡片或整个服务商当前不在看板上。 */
export interface RailAccount {
  key: string;
  card: QuotaCardId;
  name: string;
  status: CardStatus;
  hidden: boolean;
}

/**
 * 账号额度页右侧栏，兼做账号管理：
 * - 「账号及订阅服务商」按服务商列出已连接账号与状态点，点击定位到看板卡片；
 * - 「添加账号」整行 / + 都是新增一个账号，眼睛按钮单独控制该服务商是否上看板；
 * - 「更多服务商」只留外链。安装状态来自注册表扫描（后台、失败不影响操作）。
 */
function QuotaProviderRail({
  enabled,
  accounts,
  pending,
  onAdd,
  onToggleVisible,
  onFocusAccount,
}: {
  enabled: QuotaCardId[];
  accounts: RailAccount[];
  /** 正在等浏览器授权的服务商。 */
  pending: QuotaCardId | null;
  onAdd: (id: QuotaCardId) => void;
  onToggleVisible: (id: QuotaCardId) => void;
  onFocusAccount: (key: string) => void;
}) {
  const { t } = useTranslation("quota");
  const [installs, setInstalls] = useState<Record<string, InstallState>>({});

  useEffect(() => {
    let cancelled = false;
    detectInstalled().then((r) => {
      if (!cancelled) setInstalls(r);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const supported = QUOTA_PROVIDERS.filter((p) => p.card != null);
  const others = QUOTA_PROVIDERS.filter((p) => p.card == null);
  const groups = supported
    .map((p) => ({ provider: p, items: accounts.filter((a) => a.card === p.card) }))
    .filter((g) => g.items.length > 0);

  return (
    <aside className="aq-rail">
      <RailHeading text={t("railSubscriptions")} />
      {groups.length === 0 ? (
        <div className="aq-rail-empty">{t("railNoAccounts")}</div>
      ) : (
        groups.map(({ provider, items }) => (
          <div className="aq-rail-group" key={provider.id}>
            <div className="aq-rail-group-name">{provider.name}</div>
            <ul className="aq-rail-accts">
              {items.map((a) => (
                <li key={a.key}>
                  <button
                    type="button"
                    className={`aq-rail-acct${a.hidden ? " is-hidden" : ""}`}
                    onClick={() => onFocusAccount(a.key)}
                    title={a.hidden ? t("railShowAccount") : t("railFocus")}
                  >
                    <span className={`aq-dot aq-dot-${a.status}`} title={t(`status.${a.status}`)} />
                    <span className="aq-rail-acct-name">{a.name}</span>
                    {a.hidden && <EyeOff size={11} className="aq-rail-acct-hidden" aria-label={t("railHiddenMark")} />}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        ))
      )}

      <RailHeading text={t("railAddAccount")} />
      <ul className="aq-rail-list">
        {supported.map((p) => {
          const card = p.card as QuotaCardId;
          return (
            <ProviderRow
              key={p.id}
              provider={p}
              install={installs[p.id]}
              added={enabled.includes(card)}
              count={accounts.filter((a) => a.card === card).length}
              pending={pending === card}
              onAdd={() => onAdd(card)}
              onToggleVisible={() => onToggleVisible(card)}
            />
          );
        })}
      </ul>

      <RailHeading text={t("railMore")} hint={t("railMoreHint")} />
      <ul className="aq-rail-list">
        {others.map((p) => (
          <ProviderRow key={p.id} provider={p} install={installs[p.id]} added={false} />
        ))}
      </ul>
    </aside>
  );
}

function RailHeading({ text, hint }: { text: string; hint?: string }) {
  return (
    <>
      <div className="aq-rail-heading">
        <span>{text}</span>
      </div>
      {hint && <p className="aq-rail-caption">{hint}</p>}
    </>
  );
}

function ProviderRow({
  provider,
  install,
  added,
  count = 0,
  pending = false,
  onAdd,
  onToggleVisible,
}: {
  provider: QuotaProvider;
  install: InstallState | undefined;
  added: boolean;
  count?: number;
  pending?: boolean;
  onAdd?: () => void;
  onToggleVisible?: () => void;
}) {
  const { t } = useTranslation("quota");
  const notInstalled = install != null && !install.installed;
  const addTitle = t("railAddAccountTo", { name: provider.name });
  const eyeTitle = added ? t("railHide") : t("railShow");

  const openSite = (e: MouseEvent) => {
    e.stopPropagation();
    invoke("open_in_browser", { url: provider.url }).catch(() => window.open(provider.url, "_blank"));
  };

  const cls = [
    "aq-rail-row",
    onAdd ? "is-toggleable" : "is-static",
    added && "is-added",
    notInstalled && "is-missing",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <li className={cls} onClick={pending ? undefined : onAdd} title={onAdd ? addTitle : undefined}>
      {onAdd && (
        <button
          type="button"
          className="aq-rail-toggle"
          onClick={(e) => {
            e.stopPropagation();
            onAdd();
          }}
          disabled={pending}
          aria-label={addTitle}
        >
          {pending ? <RefreshCw size={13} className="spin" /> : <Plus size={13} />}
        </button>
      )}
      <span className="aq-rail-icon" aria-hidden="true">
        {install?.icon ? (
          <img src={`data:image/png;base64,${install.icon}`} alt="" draggable={false} />
        ) : (
          provider.name.charAt(0)
        )}
      </span>
      <span className="aq-rail-text">
        <span className="aq-rail-name">{provider.name}</span>
        <span className="aq-rail-domain">
          {provider.domain}
          {notInstalled && ` · ${t("railNotInstalled")}`}
        </span>
      </span>
      {count > 0 && (
        <span className="aq-rail-count" title={t("railAccountCount", { count })}>
          {count}
        </span>
      )}
      {onToggleVisible && (
        <button
          type="button"
          className={`aq-icon-btn aq-rail-eye${added ? "" : " is-off"}`}
          onClick={(e) => {
            e.stopPropagation();
            onToggleVisible();
          }}
          title={eyeTitle}
          aria-label={`${eyeTitle} ${provider.name}`}
          aria-pressed={added}
        >
          {added ? <Eye size={12} /> : <EyeOff size={12} />}
        </button>
      )}
      <button
        type="button"
        className="aq-icon-btn aq-rail-link"
        onClick={openSite}
        title={t("railOpenSite")}
        aria-label={`${t("railOpenSite")} ${provider.name}`}
      >
        <ExternalLink size={12} />
      </button>
    </li>
  );
}

export default QuotaProviderRail;
