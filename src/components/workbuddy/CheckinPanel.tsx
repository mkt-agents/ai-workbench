import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CalendarSearch, Loader2, Play, RefreshCw, Trash2 } from "lucide-react";
import {
  WB_CHECKIN_EVENT,
  WB_CHECKIN_STATUS_EVENT,
  wbCheckinLogClear,
  wbCheckinLogList,
  wbCheckinNow,
  wbCheckinStatus,
  wbUpdateSettings,
  type WbAccount,
  type WbCheckinLogEntry,
  type WbCheckinResult,
  type WbCheckinStatus,
  type WbSettings,
} from "../../lib/workbuddy";
import { useTauriEvent } from "../../hooks/useTauriEvent";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

interface Props {
  settings: WbSettings | null;
  accountCount: number;
  accounts: WbAccount[];
  onSettingsChanged: () => void;
  onAccountsChanged: () => void;
  onFlash: (kind: "success" | "error", text: string) => void;
}

let optimisticSeq = 1;

// 前端插入的进行中行没有数据库 id，用负数计数器避免与真实行撞 key。
function nextOptimisticId() {
  return -optimisticSeq++;
}

function formatWhen(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * 签到区：每日调度开关 + 时刻，加最近记录。
 *
 * 一轮签到在后端是串行、账号间 2s 间隔（防风控），所以这不是瞬时操作：
 * 按钮在整轮返回前保持禁用，同时逐账号事件会即时插进记录里。
 */
export default function CheckinPanel({
  settings,
  accountCount,
  accounts,
  onSettingsChanged,
  onAccountsChanged,
  onFlash,
}: Props) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();
  const [entries, setEntries] = useState<WbCheckinLogEntry[]>([]);
  const [running, setRunning] = useState(false);
  const [savingSchedule, setSavingSchedule] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [time, setTime] = useState(settings?.checkinTime ?? "09:30");
  const [statuses, setStatuses] = useState<Record<number, WbCheckinStatus>>({});
  const [queryingAll, setQueryingAll] = useState(false);
  const [queryingId, setQueryingId] = useState<number | null>(null);

  // 协议没配 checkin 端点时，签到必然失败（且旧构建会累计连续失败）：
  // 直接禁用入口比让人点了再看红字诚实。
  const checkinConfigured = (() => {
    const raw = settings?.adapterJson?.trim();
    if (!raw) return false;
    try {
      const parsed = JSON.parse(raw) as { checkin?: unknown } | null;
      return !!parsed?.checkin;
    } catch {
      return false;
    }
  })();

  const statusConfigured = (() => {
    const raw = settings?.adapterJson?.trim();
    if (!raw) return false;
    try {
      const parsed = JSON.parse(raw) as { checkinStatus?: unknown } | null;
      return !!parsed?.checkinStatus;
    } catch {
      return false;
    }
  })();

  useEffect(() => {
    if (settings) setTime(settings.checkinTime);
  }, [settings]);

  const refresh = useCallback(async () => {
    try {
      setEntries(await wbCheckinLogList(60));
      setError(null);
    } catch (e) {
      setError(translateError(e));
    }
  }, [translateError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 后端每完成一个账号推一次；立刻插进列表，长轮次才有实时反馈。
  useTauriEvent<WbCheckinResult>(WB_CHECKIN_EVENT, ({ payload }) => {
    setEntries((prev) => [
      {
        id: nextOptimisticId(),
        accountId: payload.accountId,
        label: payload.label,
        ok: payload.ok,
        message: payload.message,
        createdAt: payload.at,
      },
      ...prev.slice(0, 59),
    ]);
    if (payload.disabled) {
      onFlash("error", t("checkinAutoDisabled", { label: payload.label }));
      void onAccountsChanged();
    }
  });

  // 状态查询逐账号回包，实时填进状态表。
  useTauriEvent<WbCheckinStatus>(WB_CHECKIN_STATUS_EVENT, ({ payload }) => {
    setStatuses((prev) => ({ ...prev, [payload.accountId]: payload }));
  });

  const queryStatus = async (accountId: number | null) => {
    if (!statusConfigured) {
      onFlash("error", t("checkinStatusNotConfigured"));
      return;
    }
    if (accountId == null) setQueryingAll(true);
    else setQueryingId(accountId);
    setError(null);
    try {
      const results = await wbCheckinStatus(accountId);
      setStatuses((prev) => {
        const next = { ...prev };
        for (const row of results) next[row.accountId] = row;
        return next;
      });
    } catch (e) {
      setError(translateError(e));
    } finally {
      setQueryingAll(false);
      setQueryingId(null);
    }
  };

  const runAll = async () => {
    if (!checkinConfigured) {
      onFlash("error", t("checkinNoEndpoint"));
      return;
    }
    if (accountCount === 0) {
      onFlash("error", t("checkinNoAccounts"));
      return;
    }
    setRunning(true);
    setError(null);
    try {
      const results = await wbCheckinNow(null);
      const failed = results.filter((r) => !r.ok);
      if (results.length === 0) onFlash("error", t("checkinNothingToDo"));
      else if (failed.length === 0) onFlash("success", t("checkinAllOk", { count: results.length }));
      else onFlash("error", t("checkinPartial", { ok: results.length - failed.length, fail: failed.length }));
      await refresh();
      await onAccountsChanged();
    } catch (e) {
      setError(translateError(e));
    } finally {
      setRunning(false);
    }
  };

  const saveSchedule = async (enabled: boolean, nextTime: string) => {
    setSavingSchedule(true);
    setError(null);
    try {
      await wbUpdateSettings({ checkinEnabled: enabled, checkinTime: nextTime });
      onSettingsChanged();
    } catch (e) {
      setError(translateError(e));
    } finally {
      setSavingSchedule(false);
    }
  };

  const clear = async () => {
    const ok = await confirm({
      title: t("checkinClear"),
      message: t("checkinClearConfirm", { count: entries.length }),
      icon: "danger",
    });
    if (!ok) return;
    await wbCheckinLogClear().catch((e) => setError(translateError(e)));
    await refresh();
  };

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("checkinTitle")}</span>
        <div className="wb-log-tools">
          <button
            type="button"
            className="btn btn-primary btn-small"
            onClick={() => void runAll()}
            disabled={running || savingSchedule || !checkinConfigured}
            title={checkinConfigured ? undefined : t("checkinNoEndpoint")}
          >
            {running ? <Loader2 size={12} className="spin" /> : <Play size={12} />}
            <span>{running ? t("checkinRunning") : t("checkinRunAll")}</span>
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => void refresh()} title={t("logRefresh")}>
            <RefreshCw size={12} />
          </button>
          <button
            type="button"
            className="btn btn-danger btn-small"
            onClick={() => void clear()}
            disabled={entries.length === 0}
            title={t("checkinClear")}
          >
            <Trash2 size={12} />
          </button>
        </div>
      </div>
      <div className="card-body">
        <div className="wb-schedule">
          <label className="wb-check">
            <input
              type="checkbox"
              checked={settings?.checkinEnabled ?? false}
              disabled={savingSchedule || !checkinConfigured}
              onChange={(e) => void saveSchedule(e.target.checked, time)}
            />
            <span>{t("checkinDaily")}</span>
          </label>
          <input
            className="input-field wb-time-input"
            type="time"
            value={time}
            disabled={savingSchedule}
            onChange={(e) => setTime(e.target.value)}
            onBlur={() => {
              if (time !== settings?.checkinTime) void saveSchedule(settings?.checkinEnabled ?? false, time);
            }}
          />
          <span className="wb-hint-inline">
            {!checkinConfigured
              ? t("checkinNoEndpoint")
              : settings?.checkinEnabled
                ? t("checkinLastAuto", { date: settings.lastAutoCheckinDate ?? "—" })
                : t("checkinScheduleOff")}
          </span>
        </div>
        <p className="wb-hint">{t("checkinHint")}</p>

        {accounts.length > 0 && (
          <div className="wb-status-block">
            <div className="wb-status-head">
              <span className="wb-sub-text">{t("checkinStatusTitle")}</span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void queryStatus(null)}
                disabled={queryingAll || queryingId !== null || !statusConfigured}
                title={statusConfigured ? t("checkinStatusQueryAll") : t("checkinStatusNotConfigured")}
              >
                {queryingAll ? <Loader2 size={12} className="spin" /> : <CalendarSearch size={12} />}
                <span>{queryingAll ? t("checkinStatusQuerying") : t("checkinStatusQueryAll")}</span>
              </button>
            </div>
            {!statusConfigured ? (
              <p className="wb-hint">{t("checkinStatusNotConfigured")}</p>
            ) : (
              <table className="wb-table">
                <thead>
                  <tr>
                    <th>{t("label")}</th>
                    <th>{t("checkinStatusToday")}</th>
                    <th>{t("checkinStatusStreak")}</th>
                    <th>{t("checkinStatusWeek")}</th>
                    <th>{t("checkinStatusCredits")}</th>
                    <th className="wb-col-actions" />
                  </tr>
                </thead>
                <tbody>
                  {accounts.map((account) => {
                    const row = statuses[account.id];
                    const busy = queryingAll || queryingId === account.id;
                    return (
                      <tr key={account.id}>
                        <td className="wb-label-text">{account.label}</td>
                        {!row ? (
                          <td colSpan={5} className="wb-sub-text">{queryingAll ? t("checkinStatusQuerying") : "—"}</td>
                        ) : (
                          <>
                            <td>
                              {row.ok ? (
                                <span
                                  className={`wb-status wb-status-${row.todayCheckedIn ? "active" : "unverified"}`}
                                >
                                  {row.todayCheckedIn ? t("checkinStatusOk") : t("checkinStatusNo")}
                                </span>
                              ) : (
                                <span className="wb-status wb-status-expired" title={row.message ?? undefined}>
                                  {t("checkinStatusFailed")}
                                </span>
                              )}
                            </td>
                            <td className="mono">{row.streakDays ?? "—"}</td>
                            <td className="mono">{row.weekProgress ?? "—"}</td>
                            <td className="mono">{row.totalCredits ?? "—"}</td>
                          </>
                        )}
                        <td className="wb-cell-actions">
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            onClick={() => void queryStatus(account.id)}
                            disabled={busy || !statusConfigured}
                            title={t("checkinStatusQuery")}
                          >
                            {queryingId === account.id ? <Loader2 size={12} className="spin" /> : <CalendarSearch size={12} />}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        )}

        {error && <div className="wb-inline-error" role="alert">{error}</div>}

        {entries.length === 0 ? (
          <p className="wb-hint">{t("checkinEmpty")}</p>
        ) : (
          <div className="wb-log-scroll wb-checkin-scroll">
            <table className="wb-table">
              <thead>
                <tr>
                  <th>{t("logTime")}</th>
                  <th>{t("label")}</th>
                  <th>{t("logStatus")}</th>
                  <th>{t("checkinMessage")}</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((row) => (
                  <tr key={row.id}>
                    <td className="wb-cell-time">{formatWhen(row.createdAt)}</td>
                    <td>{row.label ?? `#${row.accountId}`}</td>
                    <td>
                      <span className={`wb-status wb-status-${row.ok ? "active" : "expired"}`}>
                        {row.ok ? t("checkinOk") : t("checkinFail")}
                      </span>
                    </td>
                    <td className="wb-cell-message" title={row.message ?? undefined}>
                      {row.message ?? "—"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
