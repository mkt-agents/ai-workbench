import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CalendarCheck,
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
  /** 今天的每日任务已完成（WorkBuddy 签到 / Qoder 领取）时的提示文案。 */
  dailyDone?: string;
}

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
 * - 一个列表按服务商分组：服务商行整行 / + 都是新增账号，眼睛按钮单独控制该服务商是否上看板；
 * - 已连接账号缩进挂在服务商行下（状态点，点击定位到看板卡片），有账号的服务商排前面；
 * 安装状态来自注册表扫描（后台、失败不影响操作）。
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

  const supported = QUOTA_PROVIDERS.filter((p) => p.card != null);

  const accountsOf = (card: QuotaCardId) => accounts.filter((a) => a.card === card);
  // Claude Code / Desktop 共用一张卡：账号只挂在第一个服务商下，避免同一批账号出现两次。
  const seenCards = new Set<QuotaCardId>();
  const groups = supported.map((p) => {
    const card = p.card as QuotaCardId;
    const first = !seenCards.has(card);
    seenCards.add(card);
    return { provider: p, card, items: first ? accountsOf(card) : [] };
  });
  // 有账号的服务商排前面（sort 稳定，组内保持注册顺序）。
  groups.sort((a, b) => Number(b.items.length > 0) - Number(a.items.length > 0));
  const hasAnyAccount = accounts.length > 0;

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
          {groups.map(({ provider: p }) => {
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

      {!hasAnyAccount && <p className="aq-rail-caption">{t("railNoAccounts")}</p>}
      <ul className="aq-rail-panel aq-rail-list">
        {groups.map(({ provider, card, items }) => (
          <li key={provider.id} className={`aq-rail-group${items.length > 0 ? " has-accts" : ""}`}>
            <ProviderRow
              provider={provider}
              install={installs[provider.id]}
              count={items.length}
              added={enabled.includes(card)}
              pending={pending === card}
              onAdd={() => onAdd(card)}
              onCancel={onCancelPending}
              onToggleVisible={() => onToggleVisible(card)}
            />
            {items.length > 0 && (
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
                      {a.dailyDone && (
                        <span className="aq-rail-acct-checkin" title={a.dailyDone}>
                          <CalendarCheck size={11} aria-label={a.dailyDone} />
                        </span>
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
            )}
          </li>
        ))}
      </ul>
    </aside>
  );
}

function ProviderRow({
  provider,
  install,
  count,
  added,
  pending,
  onAdd,
  onCancel,
  onToggleVisible,
}: {
  provider: QuotaProvider;
  install: InstallState | undefined;
  /** 该服务商下已连接的账号数。 */
  count: number;
  added: boolean;
  pending: boolean;
  onAdd: () => void;
  /** 授权等待中时，转圈按钮改为取消。 */
  onCancel: () => void;
  onToggleVisible: () => void;
}) {
  const { t } = useTranslation("quota");
  const notInstalled = install != null && !install.installed;
  const addTitle = t("railAddAccountTo", { name: provider.name });
  const eyeTitle = added ? t("railHide") : t("railShow");

  const cls = ["aq-rail-row", added && "is-added", notInstalled && "is-missing"].filter(Boolean).join(" ");

  return (
    <div
      className={cls}
      onClick={pending ? undefined : onAdd}
      title={`${addTitle} · ${provider.domain}`}
    >
      <span className="aq-rail-icon">
        <ProviderGlyph providerId={provider.id} name={provider.name} installIcon={install?.icon} />
        {count > 0 && (
          <span className="aq-rail-count" title={t("railAccountCount", { count })}>
            {count}
          </span>
        )}
      </span>
      <span className="aq-rail-text">
        <span className="aq-rail-name">{provider.name}</span>
        {notInstalled && <span className="aq-rail-missing">{t("railNotInstalled")}</span>}
      </span>
      <span className="aq-rail-actions">
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
        <button
          type="button"
          className={`aq-rail-toggle${pending ? " is-pending" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            if (pending) onCancel();
            else onAdd();
          }}
          title={pending ? t("railCancelAuth") : addTitle}
          aria-label={pending ? t("railCancelAuth") : addTitle}
        >
          {pending ? (
            <>
              <RefreshCw size={12} className="spin aq-rail-pending-icon" />
              <X size={13} className="aq-rail-cancel-icon" />
            </>
          ) : (
            <Plus size={13} strokeWidth={2.25} />
          )}
        </button>
      </span>
    </div>
  );
}

export default QuotaProviderRail;
