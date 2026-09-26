import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Loader2, Sprout, X } from "lucide-react";
import { wbGrowthInfo, type WbAccount, type WbGrowthInfo } from "../../lib/workbuddy";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

interface Props {
  accounts: WbAccount[];
}

/**
 * 成长计划（只读）：连登阶梯 + 今日任务。绝不自动兑换——规则禁止脚本化
 * redeem，领取动作只能回官方客户端手动做。
 */
export default function GrowthPanel({ accounts }: Props) {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();
  const [accountId, setAccountId] = useState<number | null>(accounts[0]?.id ?? null);
  const [info, setInfo] = useState<WbGrowthInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const query = async () => {
    if (accountId == null) return;
    setLoading(true);
    setError(null);
    try {
      setInfo(await wbGrowthInfo(accountId));
    } catch (e) {
      setError(translateError(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>
          <Sprout size={13} className="wb-title-icon" /> {t("growthTitle")}
        </span>
        <div className="wb-log-tools">
          <select
            className="input-field wb-log-filter"
            value={accountId ?? ""}
            onChange={(e) => setAccountId(e.target.value ? Number(e.target.value) : null)}
          >
            {accounts.map((a) => (
              <option key={a.id} value={a.id}>
                {a.label}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => void query()}
            disabled={loading || accountId == null}
          >
            {loading ? <Loader2 size={12} className="spin" /> : <Sprout size={12} />}
            <span>{loading ? t("growthQuerying") : t("growthQuery")}</span>
          </button>
        </div>
      </div>
      <div className="card-body">
        <p className="wb-hint">{t("growthRedeemNote")}</p>

        {error && <div className="wb-inline-error" role="alert">{error}</div>}

        {!info ? (
          <p className="wb-hint">{t("growthNoData")}</p>
        ) : !info.ok ? (
          <p className="wb-hint">
            {t("growthFailed")}
            {info.reasons.length > 0 && `：${info.reasons.join("；")}`}
          </p>
        ) : (
          <>
            <div className="wb-facts">
              <span>
                {t("growthStreakDays")}: <strong>{info.streak?.days ?? "—"}</strong>
              </span>
            </div>

            {info.streak && info.streak.tiers.length > 0 && (
              <table className="wb-table">
                <thead>
                  <tr>
                    <th>{t("growthTier")}</th>
                    <th>{t("growthTierRequired")}</th>
                    <th>{t("growthTierClaimed")}</th>
                  </tr>
                </thead>
                <tbody>
                  {info.streak.tiers.map((tier) => (
                    <tr key={tier.tier}>
                      <td className="mono">{tier.tier}</td>
                      <td className="mono">{tier.daysRequired}</td>
                      <td>
                        {tier.claimed == null ? (
                          <span className="wb-sub-text">—</span>
                        ) : (
                          <span
                            className={`wb-status wb-status-${tier.claimed ? "active" : "unverified"}`}
                          >
                            {tier.claimed ? <Check size={11} /> : <X size={11} />}
                            {tier.claimed ? t("growthClaimedYes") : t("growthClaimedNo")}
                          </span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            {info.tasks.length > 0 && (
              <div className="wb-growth-tasks">
                <span className="wb-sub-text">{t("growthTasks")}</span>
                <ul>
                  {info.tasks.map((task, i) => (
                    <li key={task.code ?? i}>
                      <span className="wb-growth-task-title">{task.title ?? task.code ?? "—"}</span>
                      {task.done == null ? (
                        <span className="wb-sub-text">—</span>
                      ) : (
                        <span
                          className={`wb-status wb-status-${task.done ? "active" : "unverified"}`}
                        >
                          {task.done ? t("growthTaskDone") : t("growthTaskTodo")}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            )}

            {info.reasons.length > 0 && (
              <p className="wb-hint-inline">{info.reasons.join("；")}</p>
            )}
          </>
        )}
      </div>
    </div>
  );
}
