import { useTranslation } from "react-i18next";
import type { AiHubContextInfo } from "../../core/types";

export default function ContextBudgetDetails({ info }: { info: AiHubContextInfo }) {
  const { t, i18n } = useTranslation("aihub");
  const number = (value: number) => value.toLocaleString(i18n.language);
  const budget = info.budget;
  if (!budget || info.usageKind === "compaction") {
    return <span>{t(info.usageKind === "compaction" ? "context.recalculate" : "context.historical")}</span>;
  }
  const tokens = (value: number) => t("context.tokenCount", { n: number(value) });
  const estimate = (value: number) => t("context.estimate", { n: number(value) });
  return <>
    <dl className="qa-context-facts">
      <div className="qa-context-model"><dt>{t("context.model")}</dt><dd>{budget.modelName || budget.model}</dd></div>
      <div><dt>{t("context.modelWindow")}</dt><dd>{tokens(budget.modelWindow)} <span className="qa-context-source">{t(`context.sources.${budget.windowSource}`)}</span></dd></div>
      <div><dt>{t("context.effectiveWindow")}</dt><dd>{tokens(budget.effectiveWindow)}{budget.configuredLimit != null && <span className="qa-context-source">{t("context.userCap", { n: number(budget.configuredLimit) })}</span>}</dd></div>
      <div><dt>{t("context.currentInput")}</dt><dd>{info.totalInputTokens != null ? estimate(info.totalInputTokens) : "—"}</dd></div>
      <div><dt>{t("context.toolDefinitions")}</dt><dd>{estimate(budget.schemaTokens)}</dd></div>
      <div><dt>{t("context.outputReserve")}</dt><dd>{tokens(budget.outputReserve)} <span className="qa-context-source">{t(`context.outputModes.${budget.outputMode}`)}</span></dd></div>
      <div><dt>{t("context.modelOutput")}</dt><dd>{budget.modelMaxOutputTokens != null ? tokens(budget.modelMaxOutputTokens) : t("context.unknown")} <span className="qa-context-source">{t(`context.sources.${budget.outputSource}`)}</span></dd></div>
      <div><dt>{t("context.contentBudget")}</dt><dd>{tokens(budget.inputBudget)}</dd></div>
      <div><dt>{t("context.compactTrigger")}</dt><dd>{estimate(budget.compactTrigger + budget.schemaTokens)}</dd></div>
    </dl>
    <span className="qa-context-explanation">{t("context.budgetExplanation")}</span>
    {budget.limitations.length > 0 && <div className="qa-context-adjustments" role="status">{budget.limitations.map(reason => <span key={reason}>{t(`context.limits.${reason}`, { defaultValue: t("context.adjusted") })}</span>)}</div>}
  </>;
}
