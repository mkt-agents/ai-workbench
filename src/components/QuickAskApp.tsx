import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, MessageSquarePlus, RefreshCw, Send, Sparkles, Square, X, ExternalLink } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig, AppTheme, GitRepoSummary, QuickAskChips, Snippet } from "../core/types";
import { bootApp } from "../core/boot";
import { fillParams, parseParamNames } from "../lib/snippets";
import {
  APP_THEME_CHANGED_EVENT,
  applyDocumentTheme,
  readPersistedTheme,
} from "../lib/theme";
import ModalTitleRow from "./ModalTitleRow";

const CONTEXT_LIMIT = 2000;
const SNIPPET_PICK_LIMIT = 24;
const DIRTY_FILE_LIMIT = 30;
/** Follow-up context: keep the last N Q&A pairs, clipped per turn. */
const MAX_HISTORY_TURNS = 6;
const MAX_TURN_CHARS = 4000;

type TaskKind = "none" | "debug" | "explain" | "polish";

type QaTurn = { role: "user" | "assistant"; content: string };

function clipTurn(text: string): string {
  return text.length > MAX_TURN_CHARS
    ? text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)"
    : text;
}

function appendTurn(prev: QaTurn[], question: string, answer: string): QaTurn[] {
  const next: QaTurn[] = [
    ...prev,
    { role: "user", content: clipTurn(question) },
    { role: "assistant", content: clipTurn(answer) },
  ];
  return next.slice(-MAX_HISTORY_TURNS * 2);
}

const CONTEXT_RULES =
  " Use only facts from any 'Context from workbench' section in the user message. Answer directly; do not ask the user to run git/shell commands to gather that info. If context is insufficient, say what is missing and still answer from what is given.";

const TASK_SYSTEM: Record<TaskKind, string> = {
  none:
    "You are a concise assistant for a developer workbench. Answer in the user's language." +
    CONTEXT_RULES,
  debug:
    "You diagnose errors. Be specific about likely causes and next steps. Answer in the user's language." +
    CONTEXT_RULES,
  explain:
    "You explain code or text clearly and briefly. Answer in the user's language." +
    CONTEXT_RULES,
  polish:
    "You polish and improve the user's text. Return only the improved text unless asked otherwise." +
    CONTEXT_RULES,
};

const DEFAULT_CHIPS: QuickAskChips = {
  workspace: true,
  git: true,
  dirty: true,
  clipboard: false,
};

function truncate(text: string, max: number): { text: string; truncated: boolean } {
  if (text.length <= max) return { text, truncated: false };
  return { text: text.slice(0, max) + "\n…(truncated)", truncated: true };
}

function QuickAskApp() {
  const { t, i18n } = useTranslation("quickask");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const snippets = useGlobalStore((s) => s.snippets);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitStatus = useGlobalStore((s) => s.invokeGitStatus);
  const invokeGetGitConfig = useGlobalStore((s) => s.invokeGetGitConfig);

  const [ready, setReady] = useState(false);
  const [input, setInput] = useState("");
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [task, setTask] = useState<TaskKind>("none");
  const [modelId, setModelId] = useState<string>("");
  const chips = settings.quickAskChips ?? DEFAULT_CHIPS;
  const [ctxPreview, setCtxPreview] = useState("");
  const [ctxTruncated, setCtxTruncated] = useState(false);
  const [ctxOpen, setCtxOpen] = useState(false);
  const [snippetOpen, setSnippetOpen] = useState(false);
  const [snippetQuery, setSnippetQuery] = useState("");
  const [paramTarget, setParamTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  /** Previous Q&A pairs sent along as multi-turn context (follow-up questions). */
  const [history, setHistory] = useState<QaTurn[]>([]);
  const [copied, setCopied] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);

  const setChip = (key: keyof QuickAskChips, value: boolean) => {
    setSettings({
      quickAskChips: { ...chips, [key]: value },
    });
  };

  useEffect(() => {
    let cancelled = false;
    bootApp().then(() => {
      if (!cancelled) setReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    applyDocumentTheme(settings.theme);
    if (settings.language && i18n.language !== settings.language) {
      void i18n.changeLanguage(settings.language);
    }
  }, [settings.theme, settings.language, i18n]);

  const models = aiModels as AIModelConfig[];
  const selectedModel = useMemo(() => {
    return models.find((m) => m.id === modelId) || models.find((m) => m.isDefault) || models[0];
  }, [models, modelId]);

  useEffect(() => {
    if (!modelId && selectedModel) setModelId(selectedModel.id);
  }, [modelId, selectedModel]);

  const refreshContext = useCallback(async (): Promise<{ text: string; truncated: boolean }> => {
    const parts: string[] = [];
    let truncated = false;

    if (chips.workspace) {
      const ws = gitWorkspaces[0];
      const repo = settings.currentGitRepo;
      if (ws) parts.push(`Workspace: ${ws.path}`);
      if (repo) parts.push(`Current repo: ${repo}`);
    }

    if (chips.git) {
      try {
        const [name, email] = await invokeGetGitConfig("global");
        if (name || email) parts.push(`Git identity: ${name} <${email}>`);
      } catch {
        /* ignore */
      }
    }

    if (chips.dirty) {
      const pathSet = new Set<string>();
      if (settings.currentGitRepo) pathSet.add(settings.currentGitRepo);
      for (const p of (recentProjects || []).slice(0, 8)) pathSet.add(p.path);
      const summaries: string[] = [];
      for (const path of pathSet) {
        try {
          const s: GitRepoSummary = await invokeGitRepoSummary(path);
          if (!s.isGit || s.dirtyCount <= 0) continue;
          const lines: string[] = [
            `${s.name || path} (${s.branch}): ${s.dirtyCount} changes`,
          ];
          try {
            const entries = await invokeGitStatus(path);
            const shown = entries.slice(0, DIRTY_FILE_LIMIT);
            for (const e of shown) {
              const mark =
                e.group === "untracked"
                  ? "??"
                  : `${e.indexStatus || " "}${e.workTreeStatus || " "}`;
              lines.push(`  ${mark} ${e.path}`);
            }
            if (entries.length > DIRTY_FILE_LIMIT) {
              lines.push(`  …(+${entries.length - DIRTY_FILE_LIMIT} more)`);
              truncated = true;
            }
          } catch {
            /* summary line alone is still useful */
          }
          summaries.push(lines.join("\n"));
        } catch {
          /* ignore */
        }
        if (summaries.length >= 5) break;
      }
      if (summaries.length) parts.push("Dirty repos:\n" + summaries.join("\n\n"));
    }

    if (chips.clipboard) {
      try {
        const clip = await invoke<string>("read_clipboard");
        if (clip.trim()) {
          const tclip = truncate(clip.trim(), 800);
          parts.push("Clipboard:\n" + tclip.text);
          if (tclip.truncated) truncated = true;
        }
      } catch {
        /* ignore */
      }
    }

    const joined = parts.join("\n\n");
    const result = truncate(joined, CONTEXT_LIMIT);
    const out = { text: result.text, truncated: truncated || result.truncated };
    setCtxPreview(out.text);
    setCtxTruncated(out.truncated);
    return out;
  }, [
    chips.workspace,
    chips.git,
    chips.dirty,
    chips.clipboard,
    gitWorkspaces,
    recentProjects,
    settings.currentGitRepo,
    invokeGetGitConfig,
    invokeGitRepoSummary,
    invokeGitStatus,
  ]);

  // Use ref to always have the latest refreshContext without re-registering listeners
  const refreshContextRef = useRef(refreshContext);
  refreshContextRef.current = refreshContext;
  // Deduplicate prefill events (both quick-ask windows receive the same event)
  const prefillIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (!ready) return;
    void refreshContextRef.current();
  }, [ready]);

  useEffect(() => {
    let unlistenShown: (() => void) | undefined;
    let unlistenPrefill: (() => void) | undefined;
    let unlistenTheme: (() => void) | undefined;

    const syncThemeFromStorage = () => {
      const theme = readPersistedTheme();
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    };

    void listen("quick-ask-shown", () => {
      syncThemeFromStorage();
      void refreshContextRef.current();
    }).then((fn) => {
      unlistenShown = fn;
    });
    void listen<{ id?: string; text?: string }>("quick-ask-prefill", (event) => {
      const payload = event.payload;
      let text = "";
      let id: string | null = null;
      if (typeof payload === "string") {
        text = payload;
      } else if (payload && typeof payload === "object") {
        text = (payload.text as string) || "";
        id = (payload.id as string) || null;
      }
      if (!text) return;
      // Deduplicate: if this event was already processed, skip
      if (id && prefillIdRef.current === id) return;
      if (id) {
        prefillIdRef.current = id;
      }
      setInput((prev) => (prev ? `${prev}\n${text}` : text));
      setSnippetOpen(false);
      setParamTarget(null);
      requestAnimationFrame(() => inputRef.current?.focus());
    }).then((fn) => {
      unlistenPrefill = fn;
    });
    void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
      const theme = event.payload;
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    }).then((fn) => {
      unlistenTheme = fn;
    });
    return () => {
      unlistenShown?.();
      unlistenPrefill?.();
      unlistenTheme?.();
    };
  }, [setSettings]);

  const hide = async () => {
    // Go through Rust: tao resolves hide() via its cached visibility flag, which can
    // desync from the real window (see tray/bubble.rs `vis`) and turn this into a
    // no-op. Fall back to the JS API if the command is unavailable.
    try {
      await invoke("hide_quick_ask");
    } catch {
      try {
        await getCurrentWindow().hide();
      } catch {
        /* ignore */
      }
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      // A modal on top wins over hiding the window.
      if (paramTarget) {
        setParamTarget(null);
        return;
      }
      void hide();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paramTarget]);

  const fillClipboard = async () => {
    try {
      const clip = await invoke<string>("read_clipboard");
      if (clip.trim()) setInput((prev) => (prev ? prev : clip.trim()));
    } catch (e) {
      setError(String(e));
    }
  };

  const applySnippetText = async (text: string, snippetId: string) => {
    setInput((prev) => (prev ? `${prev}\n${text}` : text));
    setSnippetOpen(false);
    setParamTarget(null);
    setSnippetQuery("");
    await bumpSnippetUse(snippetId);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const pickSnippet = (s: Snippet) => {
    const names = parseParamNames(s.params, s.content);
    if (names.length === 0) {
      void applySnippetText(s.content, s.id);
      return;
    }
    const init: Record<string, string> = {};
    names.forEach((n) => {
      init[n] = "";
    });
    setParamValues(init);
    setParamTarget(s);
  };

  const confirmSnippetParams = async () => {
    if (!paramTarget) return;
    const text = fillParams(paramTarget.content, paramValues);
    await applySnippetText(text, paramTarget.id);
  };

  const snippetChoices = useMemo(() => {
    const q = snippetQuery.trim().toLowerCase();
    const list = [...(snippets || [])].sort((a, b) => b.useCount - a.useCount || b.updatedAt.localeCompare(a.updatedAt));
    const filtered = q
      ? list.filter(
          (s) =>
            s.name.toLowerCase().includes(q) ||
            s.tags.toLowerCase().includes(q) ||
            s.content.toLowerCase().includes(q)
        )
      : list;
    return filtered.slice(0, SNIPPET_PICK_LIMIT);
  }, [snippets, snippetQuery]);

  const send = async (rawInput?: string, historyOverride?: QaTurn[]) => {
    if (busy) return;
    if (!selectedModel) {
      setError(t("noModel"));
      return;
    }
    const question = (rawInput ?? input).trim();
    if (!question) {
      setError(t("emptyInput"));
      return;
    }
    // The context dump is per-request; history carries only the plain Q&A pairs.
    const baseHistory = historyOverride ?? history;
    const reqId = `qa-${Date.now()}-${++streamIdRef.current}`;
    const myGen = streamIdRef.current;
    activeReqRef.current = reqId;
    setBusy(true);
    setError(null);
    setAnswer("");

    let settled = false;
    let full = "";
    const teardown = () => {
      unlisteners.forEach((fn) => fn());
      unlisteners.length = 0;
    };
    const unlisteners: Array<() => void> = [];

    try {
      const ctxResult = await refreshContext();
      const system = TASK_SYSTEM[task];
      const ctx = ctxResult.text;
      const user = ctx
        ? `${question}\n\n---\nContext from workbench:\n${ctx}`
        : question;

      const onChunk = async (event: { payload?: { id: string; text: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        full += payload.text || "";
        setAnswer((prev) => prev + (payload.text || ""));
      };
      const onDone = async (event: { payload?: { id: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          setBusy(false);
          // Completed exchange becomes follow-up context (plain text, no ctx dump).
          if (full.trim()) {
            setHistory(appendTurn(baseHistory, question, full));
            lastQuestionRef.current = question;
            lastHistoryRef.current = baseHistory;
          }
        }
        activeReqRef.current = null;
        teardown();
      };
      const onError = async (event: { payload?: { id: string; error: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          // A user-triggered stop is not a failure — keep the partial answer.
          const stopped = (payload.error || "").includes("AI_REQUEST_CANCELLED");
          if (!stopped) setError(payload.error || t("streamFailed"));
          else if (full.trim()) {
            // Stopped mid-stream: keep the partial answer as context too.
            setHistory(appendTurn(baseHistory, question, full));
            lastQuestionRef.current = question;
            lastHistoryRef.current = baseHistory;
          }
          setBusy(false);
        }
        activeReqRef.current = null;
        teardown();
      };

      unlisteners.push(await listen<{ id: string; text: string }>("generate-text-chunk", onChunk));
      unlisteners.push(await listen<{ id: string }>("generate-text-done", onDone));
      unlisteners.push(await listen<{ id: string; error: string }>("generate-text-error", onError));

      const maxTokens = Math.max(selectedModel.maxTokens || 0, 1536);
      await invoke("generate_text_stream", {
        req: {
          config: { ...selectedModel, maxTokens },
          system,
          user,
          requestId: reqId,
          history: baseHistory,
        },
      });
      if (streamIdRef.current === myGen) {
        setInput("");
        lastQuestionRef.current = question;
        lastHistoryRef.current = baseHistory;
      }
    } catch (e) {
      if (streamIdRef.current === myGen && !settled) {
        settled = true;
        // Stopping rejects the command too — no error for a deliberate stop.
        if (!String(e).includes("AI_REQUEST_CANCELLED")) {
          setError(String(e));
        }
        setBusy(false);
      }
      activeReqRef.current = null;
      teardown();
    }
  };

  const startNewChat = () => {
    if (busy) return;
    setHistory([]);
    setAnswer("");
    setError(null);
    lastQuestionRef.current = "";
    lastHistoryRef.current = [];
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  /// Abort the in-flight stream. The backend answers with an
  /// AI_REQUEST_CANCELLED error event (handled as a normal stop) and the partial
  /// answer stays on screen.
  const stopStream = async () => {
    const id = activeReqRef.current;
    if (!id) return;
    activeReqRef.current = null;
    try {
      await invoke("cancel_request", { id });
    } catch (e) {
      setError(String(e));
    }
  };

  // Cleanup any live stream listeners if the window closes mid-stream.
  useEffect(() => {
    return () => {
      // Bump the generation counter so any in-flight callbacks become no-ops.
      streamIdRef.current++;
    };
  }, []);

  const copyAnswer = async () => {
    if (!answer) return;
    try {
      await invokeCopyToClipboard(answer);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const openDeepSeek = async () => {
    try {
      await invoke("open_main_deepseek");
      await hide();
    } catch (e) {
      setError(String(e));
    }
  };

  if (!ready) {
    return <div className="quick-ask-root loading">{t("loading")}</div>;
  }

  return (
    <div className="quick-ask-root">
      <header className="quick-ask-header" data-tauri-drag-region>
        <div className="quick-ask-title" data-tauri-drag-region>
          <Sparkles size={16} />
          <span>{t("title")}</span>
        </div>
        <button type="button" className="btn-icon" onClick={() => void hide()} title={t("close")}>
          <X size={16} />
        </button>
      </header>

      <div className="quick-ask-toolbar">
        <select
          className="input-field"
          value={selectedModel?.id || ""}
          onChange={(e) => setModelId(e.target.value)}
        >
          {models.length === 0 && <option value="">{t("noModel")}</option>}
          {models.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name} ({m.model})
            </option>
          ))}
        </select>
        <select
          className="input-field"
          value={task}
          onChange={(e) => setTask(e.target.value as TaskKind)}
        >
          <option value="none">{t("task.none")}</option>
          <option value="debug">{t("task.debug")}</option>
          <option value="explain">{t("task.explain")}</option>
          <option value="polish">{t("task.polish")}</option>
        </select>
      </div>

      <div className="quick-ask-chips">
        <label className={`qa-chip ${chips.workspace ? "on" : ""}`}>
          <input
            type="checkbox"
            checked={chips.workspace}
            onChange={(e) => setChip("workspace", e.target.checked)}
          />
          {t("chip.workspace")}
        </label>
        <label className={`qa-chip ${chips.git ? "on" : ""}`}>
          <input type="checkbox" checked={chips.git} onChange={(e) => setChip("git", e.target.checked)} />
          {t("chip.git")}
        </label>
        <label className={`qa-chip ${chips.dirty ? "on" : ""}`}>
          <input type="checkbox" checked={chips.dirty} onChange={(e) => setChip("dirty", e.target.checked)} />
          {t("chip.dirty")}
        </label>
        <label className={`qa-chip ${chips.clipboard ? "on" : ""}`}>
          <input
            type="checkbox"
            checked={chips.clipboard}
            onChange={(e) => setChip("clipboard", e.target.checked)}
          />
          {t("chip.clipboard")}
        </label>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => void fillClipboard()}>
          {t("pasteClipboard")}
        </button>
        {(snippets?.length ?? 0) > 0 && (
          <button type="button" className="btn btn-secondary btn-small" onClick={() => setSnippetOpen((v) => !v)}>
            {t("snippets")}
          </button>
        )}
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => setCtxOpen((v) => !v)}
          disabled={!ctxPreview}
        >
          {ctxOpen ? t("hideContext") : t("showContext")}
        </button>
      </div>

      {ctxTruncated && <div className="quick-ask-hint">{t("truncated")}</div>}
      {ctxOpen && ctxPreview && <pre className="quick-ask-ctx-preview">{ctxPreview}</pre>}

      {snippetOpen && (
        <div className="quick-ask-snippets">
          <input
            className="input-field quick-ask-snippet-search"
            placeholder={t("snippetSearch")}
            value={snippetQuery}
            onChange={(e) => setSnippetQuery(e.target.value)}
            autoFocus
          />
          <div className="quick-ask-snippet-list">
            {snippetChoices.map((s) => (
              <button key={s.id} type="button" className="qa-snippet-item" onClick={() => pickSnippet(s)}>
                {s.name}
              </button>
            ))}
            {snippetChoices.length === 0 && <span className="quick-ask-hint">{t("snippetEmpty")}</span>}
          </div>
        </div>
      )}

      <textarea
        ref={inputRef}
        className="quick-ask-input input-field"
        placeholder={t("placeholder")}
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== "Enter") return;
          if (e.shiftKey) return; // Shift+Enter = newline
          e.preventDefault();
          if (!busy) void send();
        }}
        rows={5}
      />

      <div className="quick-ask-actions">
        {busy ? (
          <button type="button" className="btn btn-secondary" onClick={() => void stopStream()}>
            <Square size={14} />
            {t("stop")}
          </button>
        ) : (
          <button type="button" className="btn btn-primary" onClick={() => void send()}>
            <Send size={14} />
            {t("send")}
          </button>
        )}
        <button
          type="button"
          className="btn btn-secondary"
          disabled={busy || !lastQuestionRef.current}
          onClick={() => void send(lastQuestionRef.current, lastHistoryRef.current)}
          title={t("regenerate")}
        >
          <RefreshCw size={14} /> {t("regenerate")}
        </button>
        <button type="button" className="btn btn-secondary" disabled={!answer} onClick={() => void copyAnswer()}>
          {copied ? <Check size={14} /> : <Copy size={14} />} {copied ? t("copied") : t("copy")}
        </button>
        <button type="button" className="btn btn-secondary" onClick={() => void openDeepSeek()}>
          <ExternalLink size={14} /> {t("openDeepseek")}
        </button>
        {(history.length > 0 || answer) && (
          <button
            type="button"
            className="btn btn-secondary"
            disabled={busy}
            onClick={startNewChat}
            title={t("newChat")}
          >
            <MessageSquarePlus size={14} /> {t("newChat")}
          </button>
        )}
      </div>

      {error && <div className="quick-ask-error">{error}</div>}

      {(answer || busy) && (
        <pre className={`quick-ask-answer${busy ? " is-streaming" : ""}`}>
          {answer}
          {busy ? "▍" : ""}
        </pre>
      )}

      {paramTarget && (
        <div className="modal-overlay" onClick={() => setParamTarget(null)}>
          <div className="modal snippets-modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow title={t("fillParams")} onClose={() => setParamTarget(null)} />
            {Object.keys(paramValues).map((key) => (
              <div className="input-group" key={key}>
                <label className="input-label">{key}</label>
                <input
                  className="input-field"
                  value={paramValues[key]}
                  onChange={(e) => setParamValues({ ...paramValues, [key]: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void confirmSnippetParams();
                    }
                  }}
                  autoFocus={Object.keys(paramValues)[0] === key}
                />
              </div>
            ))}
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setParamTarget(null)}>
                {t("cancel")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => void confirmSnippetParams()}>
                {t("insertSnippet")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default QuickAskApp;
