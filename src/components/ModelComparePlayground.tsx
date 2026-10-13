import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Columns2, Loader2, Play, RotateCcw } from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig } from "../core/types";

type CompareResult = {
  id: string;
  name: string;
  status: "pending" | "ok" | "fail";
  text: string;
  elapsedMs: number;
  error?: string;
};

const MAX_PICK = 3;

/** One-shot side-by-side model compare (not a chat page). */
export default function ModelComparePlayground() {
  const { t } = useTranslation("ai");
  const aiModels = useGlobalStore((s) => s.aiModels);
  const invokeGenerateText = useGlobalStore((s) => s.invokeGenerateText);

  const [prompt, setPrompt] = useState("");
  const [system, setSystem] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<CompareResult[]>([]);

  const byId = useMemo(() => {
    const map = new Map<string, AIModelConfig>();
    for (const m of aiModels) map.set(m.id, m);
    return map;
  }, [aiModels]);

  const canRun = !busy && !!prompt.trim() && picked.length > 0;
  const canClear =
    !busy &&
    (!!prompt.trim() || !!system.trim() || picked.length > 0 || results.length > 0);
  const colClass = `cols-${Math.min(Math.max(results.length, 1), 3)}` as const;
  const pendingCount = results.filter((r) => r.status === "pending").length;

  const toggle = (id: string) => {
    setPicked((prev) => {
      if (prev.includes(id)) return prev.filter((x) => x !== id);
      if (prev.length >= MAX_PICK) return prev;
      return [...prev, id];
    });
  };

  const clearAll = () => {
    if (busy) return;
    setPrompt("");
    setSystem("");
    setPicked([]);
    setResults([]);
  };

  const patchResult = (next: CompareResult) => {
    setResults((prev) => prev.map((r) => (r.id === next.id ? next : r)));
  };

  const run = async () => {
    const q = prompt.trim();
    if (!q || picked.length === 0 || busy) return;
    setBusy(true);

    const placeholders: CompareResult[] = picked.map((id) => {
      const config = byId.get(id);
      return {
        id,
        name: config?.name || id,
        status: "pending",
        text: "",
        elapsedMs: 0,
      };
    });
    setResults(placeholders);

    const sys = system.trim() || t("models.compareDefaultSystem");
    await Promise.all(
      placeholders.map(async (slot) => {
        const config = byId.get(slot.id);
        if (!config) {
          patchResult({
            ...slot,
            status: "fail",
            error: t("models.compareMissingConfig"),
          });
          return;
        }
        const started = performance.now();
        try {
          const text = await invokeGenerateText({
            config: { ...config, maxTokens: Math.max(config.maxTokens || 0, 1024) },
            system: sys,
            user: q,
          });
          patchResult({
            id: slot.id,
            name: config.name,
            status: "ok",
            text: text.trim(),
            elapsedMs: Math.round(performance.now() - started),
          });
        } catch (e) {
          patchResult({
            id: slot.id,
            name: config.name,
            status: "fail",
            text: "",
            elapsedMs: Math.round(performance.now() - started),
            error: String(e),
          });
        }
      })
    );
    setBusy(false);
  };

  return (
    <div className="model-compare">
      <p className="model-compare-hint">{t("models.compareHint")}</p>

      <section className="model-compare-composer">
        <label className="model-compare-field">
          <span>{t("models.comparePrompt")}</span>
          <textarea
            className="input-field model-compare-prompt"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            rows={3}
            placeholder={t("models.comparePromptPlaceholder")}
          />
        </label>

        <label className="model-compare-field">
          <span>{t("models.compareSystem")}</span>
          <input
            className="input-field"
            value={system}
            onChange={(e) => setSystem(e.target.value)}
            placeholder={t("models.compareSystemPlaceholder")}
          />
        </label>

        <div className="model-compare-pick">
          <div className="model-compare-pick-label">
            {t("models.comparePick", { max: MAX_PICK })}
            <span className="model-compare-pick-count">
              {picked.length}/{MAX_PICK}
            </span>
          </div>
          <div className="model-compare-chips">
            {aiModels.map((m) => {
              const on = picked.includes(m.id);
              return (
                <button
                  key={m.id}
                  type="button"
                  className={`model-compare-chip${on ? " is-on" : ""}`}
                  onClick={() => toggle(m.id)}
                  disabled={busy || (!on && picked.length >= MAX_PICK)}
                  title={m.model}
                >
                  {m.name}
                </button>
              );
            })}
            {aiModels.length === 0 && (
              <span className="runtime-muted">{t("models.noModels")}</span>
            )}
          </div>
        </div>

        <div className="model-compare-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={clearAll}
            disabled={!canClear}
            title={t("models.compareClearAll")}
          >
            <RotateCcw size={12} />
            {t("models.compareClearAll")}
          </button>
          <button
            type="button"
            className="btn btn-primary btn-small"
            disabled={!canRun}
            onClick={() => void run()}
          >
            {busy ? <Loader2 size={14} className="spin" /> : <Play size={14} />}
            {busy
              ? pendingCount > 0
                ? t("models.compareProgress", {
                    done: results.length - pendingCount,
                    total: results.length,
                  })
                : t("models.compareRunning")
              : t("models.compareRun")}
          </button>
        </div>
      </section>

      <section className="model-compare-results" aria-live="polite">
        {results.length > 0 ? (
          <div className={`model-compare-grid ${colClass}`}>
            {results.map((r) => (
              <article
                key={r.id}
                className={`model-compare-card${
                  r.status === "fail" ? " is-fail" : r.status === "pending" ? " is-pending" : ""
                }`}
              >
                <header>
                  <strong title={r.name}>{r.name}</strong>
                  {r.status === "pending" ? (
                    <span className="model-compare-latency is-pending">
                      <Loader2 size={11} className="spin" />
                      {t("models.comparePending")}
                    </span>
                  ) : (
                    <span className={`model-compare-latency${r.status === "fail" ? " is-fail" : ""}`}>
                      {r.elapsedMs >= 1000
                        ? t("models.compareElapsedSec", { n: (r.elapsedMs / 1000).toFixed(1) })
                        : t("models.compareElapsedMs", { n: r.elapsedMs })}
                    </span>
                  )}
                </header>
                {r.status === "pending" ? (
                  <div className="model-compare-pending">
                    <Loader2 size={18} className="spin" />
                    <span>{t("models.comparePendingHint")}</span>
                  </div>
                ) : r.status === "ok" ? (
                  <pre className="model-compare-out">{r.text || "—"}</pre>
                ) : (
                  <p className="model-compare-err">{r.error}</p>
                )}
              </article>
            ))}
          </div>
        ) : (
          <div className="model-compare-empty">
            <div className="model-compare-empty-icon" aria-hidden>
              <Columns2 size={22} />
            </div>
            <div className="model-compare-empty-title">{t("models.compareEmptyTitle")}</div>
            <div className="model-compare-empty-desc">{t("models.compareEmptyDesc")}</div>
          </div>
        )}
      </section>
    </div>
  );
}
