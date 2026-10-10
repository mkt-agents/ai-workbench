import { useEffect, useState, type MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  CalendarCheck,
  ChevronRight,
  ExternalLink,
  Eye,
  EyeOff,
  PanelRightClose,
  PanelRightOpen,
  Plus,
  RefreshCw,
  Trash2,
  X,
} from "lucide-react";
import {
  detectInstalled,
  QUOTA_PROVIDERS,
  type InstallState,
  type QuotaCardId,
  type QuotaProvider,
} from "../lib/quotaProviders";
import { providerLogoSrc } from "../lib/providerLogos";
import type { CardStatus } from "../lib/quotaCache";

/** 本机安装图标优先，否则用打包的品牌图，再否则首字母。 */
function ProviderGlyph({
  providerId,
  name,
  installIcon,
}: {
  providerId: string;
  name: string;
  installIcon?: string | null;
}) {
  if (installIcon) {
    return <img src={`data:image/png;base64,${installIcon}`} alt="" draggable={false} />;
  }
  const logo = providerLogoSrc(providerId);
  if (logo) {
    return <img src={logo} alt="" draggable={false} />;
  }
  return <span aria-hidden="true">{name.charAt(0)}</span>;
}

/** 右侧栏里的一个已连接账号；`hidden` = 卡片或整个服务商当前不在看板上。 */
export interface RailAccount {
  key: string;
  card: QuotaCardId;
  name: string;
  status: CardStatus;
  hidden: boolean;
  /** 仅 WorkBuddy：今天已签到。 */
  checkedIn?: boolean;
}

const MORE_KEY = "aq-rail-more";
/** 与 CSS 里栏目排到看板下方的断点一致；窄窗下忽略收起状态。 */
const NARROW_QUERY = "(max-width: 900px)";
/** 收起态图标上的状态点取最差的一个账号。 */
const STATUS_RANK: CardStatus[] = ["reauth", "error", "bad", "warn", "ok", "idle"];

function worstStatus(list: RailAccount[]): CardStatus | null {
  if (list.length === 0) return null;
  return STATUS_RANK.find((s) => list.some((a) => a.status === s)) ?? null;
}

function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const mql = window.matchMedia(query);
    const onChange = () => setMatches(mql.matches);
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

/**
 * 账号额度页右侧栏，兼做账号管理：
 * - 「账号及订阅服务商」按服务商列出已连接账号与状态点，点击定位到看板卡片；
 * - 「添加账号」整行 / + 都是新增一个账号，眼睛按钮单独控制该服务商是否上看板；
 * - 「更多服务商」只留外链，默认折叠。安装状态来自注册表扫描（后台、失败不影响操作）。
 * 整栏可收起成图标条，图标点击同样是添加账号。
 */
function QuotaProviderRail({
  enabled,
  accounts,
  pending,
  collapsed,
  onToggleCollapsed,
  onAdd,
  onCancelPending,
  onToggleVisible,
  onFocusAccount,
  onDeleteAccount,
}: {
  enabled: QuotaCardId[];
  accounts: RailAccount[];
  /** 正在等浏览器授权的服务商。 */
  pending: QuotaCardId | null;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onAdd: (id: QuotaCardId) => void;
  onCancelPending: () => void;
  onToggleVisible: (id: QuotaCardId) => void;
  onFocusAccount: (key: string) => void;
  /** 删除本机保存的账号（调用方负责二次确认）。 */
  onDeleteAccount: (key: string) => void;
}) {
  const { t } = useTranslation("quota");
  const [installs, setInstalls] = useState<Record<string, InstallState>>({});
  const [moreOpen, setMoreOpen] = useState(() => localStorage.getItem(MORE_KEY) === "1");
  const narrow = useMediaQuery(NARROW_QUERY);

  useEffect(() => {
    let cancelled = false;
    detectInstalled().then((r) => {
      if (!cancelled) setInstalls(r);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleMore = () => {
    setMoreOpen((open) => {
      try {
        localStorage.setItem(MORE_KEY, open ? "0" : "1");
      } catch {
        /* private mode / quota — the section just reopens collapsed */
      }
      return !open;
    });
  };

  const supported = QUOTA_PROVIDERS.filter((p) => p.card != null);
  const others = QUOTA_PROVIDERS.filter((p) => p.card == null);
  const accountsOf = (card: QuotaCardId) => accounts.filter((a) => a.card === card);
  // Claude Code / Desktop 共用一张卡：账号分组按 card 去重，避免同一批账号出现两次。
  const seenCards = new Set<QuotaCardId>();
  const groups = supported
    .filter((p) => {
      const card = p.card as QuotaCardId;
      if (seenCards.has(card)) return false;
      seenCards.add(card);
      return true;
    })
    .map((p) => ({ provider: p, items: accountsOf(p.card as QuotaCardId) }))
    .filter((g) => g.items.length > 0);

  if (collapsed && !narrow) {
    return (
      <aside className="aq-rail is-collapsed">
        <button
          type="button"
          className="aq-icon-btn aq-rail-collapse"
          onClick={onToggleCollapsed}
          title={t("railExpand")}
          aria-label={t("railExpand")}
        >
          <PanelRightOpen size={14} />
        </button>
        <ul className="aq-rail-mini-list">
          {supported.map((p) => {
            const card = p.card as QuotaCardId;
            const items = accountsOf(card);
            const status = worstStatus(items);
            const icon = installs[p.id]?.icon;
            const isPending = pending === card;
            const title = isPending
              ? t("railCancelAuth")
              : [
                  p.name,
                  items.length > 0 ? t("railAccountCount", { count: items.length }) : null,
                  status ? t(`status.${status}`) : null,
                  t("railAddAccountTo", { name: p.name }),
                ]
                  .filter(Boolean)
                  .join("\n");
            return (
              <li key={p.id}>
                <button
                  type="button"
                  className={`aq-rail-mini${enabled.includes(card) ? "" : " is-off"}${isPending ? " is-pending" : ""}`}
                  onClick={() => (isPending ? onCancelPending() : onAdd(card))}
                  title={title}
                  aria-label={isPending ? t("railCancelAuth") : t("railAddAccountTo", { name: p.name })}
                >
                  <span className="aq-rail-mini-icon">
                    {isPending ? (
                      <RefreshCw size={12} className="spin" />
                    ) : (
                      <ProviderGlyph providerId={p.id} name={p.name} installIcon={icon} />
                    )}
                  </span>
                  {status && !isPending && <span className={`aq-dot aq-dot-${status}`} />}
                </button>
              </li>
            );
          })}
        </ul>
      </aside>
    );
  }

  return (
    <aside className="aq-rail">
      <header className="aq-rail-head">
        <h2 className="aq-rail-title">{t("railSubscriptions")}</h2>
        <button
          type="button"
          className="aq-icon-btn aq-rail-collapse"
          onClick={onToggleCollapsed}
          title={t("railCollapse")}
          aria-label={t("railCollapse")}
        >
          <PanelRightClose size={13} />
        </button>
      </header>

      {groups.length === 0 ? (
        <div className="aq-rail-empty">{t("railNoAccounts")}</div>
      ) : (
        <div className="aq-rail-panel aq-rail-accounts">
          {groups.map(({ provider, items }) => (
            <div key={provider.id} className="aq-rail-group">
              <div className="aq-rail-group-name">
                <span className="aq-rail-group-icon">
                  <ProviderGlyph
                    providerId={provider.id}
                    name={provider.name}
                    installIcon={installs[provider.id]?.icon}
                  />
                </span>
                <span className="aq-rail-group-label">{provider.name}</span>
                <span className="aq-rail-group-count">{items.length}</span>
              </div>
              <ul className="aq-rail-accts">
                {items.map((a) => (
                  <li key={a.key} className="aq-rail-acct-row">
                    <button
                      type="button"
                      className={`aq-rail-acct${a.hidden ? " is-hidden" : ""}`}
                      onClick={() => onFocusAccount(a.key)}
                      title={a.hidden ? t("railShowAccount") : t("railFocus")}
                    >
                      <span className={`aq-dot aq-dot-${a.status}`} title={t(`status.${a.status}`)} />
                      <span className="aq-rail-acct-name">{a.name}</span>
                      {a.checkedIn && (
                        <CalendarCheck size={11} className="aq-rail-acct-checkin" aria-label={t("checkinDoneShort")} />
                      )}
                      {a.hidden && <EyeOff size={11} className="aq-rail-acct-hidden" aria-label={t("railHiddenMark")} />}
                    </button>
                    <button
                      type="button"
                      className="aq-icon-btn aq-icon-btn-danger aq-rail-acct-del"
                      onClick={() => onDeleteAccount(a.key)}
                      title={t("removeAuth")}
                      aria-label={`${t("removeAuth")} ${a.name}`}
                    >
                      <Trash2 size={11} />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      )}

      <div className="aq-rail-section-label">{t("railAddAccount")}</div>
      <ul className="aq-rail-panel aq-rail-list">
        {supported.map((p) => {
          const card = p.card as QuotaCardId;
          return (
            <ProviderRow
              key={p.id}
              provider={p}
              install={installs[p.id]}
              added={enabled.includes(card)}
              pending={pending === card}
              onAdd={() => onAdd(card)}
              onCancel={onCancelPending}
              onToggleVisible={() => onToggleVisible(card)}
            />
          );
        })}
      </ul>

      <button
        type="button"
        className={`aq-rail-more-toggle${moreOpen ? " is-open" : ""}`}
        onClick={toggleMore}
        aria-expanded={moreOpen}
        title={t("railMoreToggle")}
      >
        <ChevronRight size={12} className="aq-rail-chevron" />
        <span>
          {t("railMore")} · {others.length}
        </span>
      </button>
      {moreOpen && (
        <div className="aq-rail-more">
          <p className="aq-rail-caption">{t("railMoreHint")}</p>
          <ul className="aq-rail-panel aq-rail-list is-static-list">
            {others.map((p) => (
              <ProviderRow key={p.id} provider={p} install={installs[p.id]} added={false} />
            ))}
          </ul>
        </div>
      )}
    </aside>
  );
}

function ProviderRow({
  provider,
  install,
  added,
  pending = false,
  onAdd,
  onCancel,
  onToggleVisible,
}: {
  provider: QuotaProvider;
  install: InstallState | undefined;
  added: boolean;
  pending?: boolean;
  onAdd?: () => void;
  /** 授权等待中时，转圈按钮改为取消。 */
  onCancel?: () => void;
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
    <li
      className={cls}
      onClick={pending ? undefined : onAdd}
      title={onAdd ? `${addTitle} · ${provider.domain}` : provider.domain}
    >
      {onAdd && (
        <button
          type="button"
          className={`aq-rail-toggle${pending ? " is-pending" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            if (pending) onCancel?.();
            else onAdd();
          }}
          title={pending ? t("railCancelAuth") : undefined}
          aria-label={pending ? t("railCancelAuth") : addTitle}
        >
          {pending ? (
            <>
              <RefreshCw size={12} className="spin aq-rail-pending-icon" />
              <X size={13} className="aq-rail-cancel-icon" />
            </>
          ) : (
            <Plus size={14} strokeWidth={2.25} />
          )}
        </button>
      )}
      <span className="aq-rail-icon">
        <ProviderGlyph providerId={provider.id} name={provider.name} installIcon={install?.icon} />
      </span>
      <span className="aq-rail-text">
        <span className="aq-rail-name">{provider.name}</span>
        {notInstalled && <span className="aq-rail-missing">{t("railNotInstalled")}</span>}
      </span>
      <span className="aq-rail-actions">
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
            {added ? <Eye size={13} /> : <EyeOff size={13} />}
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
      </span>
    </li>
  );
}

export default QuotaProviderRail;
