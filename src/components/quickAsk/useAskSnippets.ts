import { useCallback, useMemo, useState } from "react";
import { useGlobalStore } from "../../core/store";
import type { Snippet } from "../../core/types";
import { fillParams, parseParamNames } from "../../lib/snippets";
import { SNIPPET_PICK_LIMIT } from "./config";

/** Match a trailing `/query` on the last line (Continue-style slash insert). */
export function matchSlashCommand(input: string): { before: string; query: string } | null {
  const m = /(?:^|\n)\/([^\n]*)$/.exec(input);
  if (!m) return null;
  const start = m.index + (m[0].startsWith("\n") ? 1 : 0);
  return { before: input.slice(0, start), query: m[1] };
}

export type AskSnippetsOptions = {
  setInput: (updater: string | ((prev: string) => string)) => void;
  inputRef: { current: HTMLTextAreaElement | null };
  /** Current composer text — used to replace a trailing `/query` on pick. */
  input?: string;
};

/** Snippet picker: search, param-fill modal, insert into the input box. */
export function useAskSnippets(opts: AskSnippetsOptions) {
  const { setInput, inputRef, input = "" } = opts;
  const snippets = useGlobalStore((s) => s.snippets);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);

  const [snippetOpen, setSnippetOpen] = useState(false);
  const [snippetQuery, setSnippetQuery] = useState("");
  const [paramTarget, setParamTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  /** When true, the picker was opened by typing `/` (replace slash token on pick). */
  const [slashMode, setSlashMode] = useState(false);

  const applySnippetText = useCallback(async (text: string, snippetId: string) => {
    setInput((prev) => {
      if (slashMode) {
        const hit = matchSlashCommand(prev);
        if (hit) return hit.before + text;
      }
      return prev ? `${prev}\n${text}` : text;
    });
    setSnippetOpen(false);
    setSlashMode(false);
    setParamTarget(null);
    setSnippetQuery("");
    await bumpSnippetUse(snippetId);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [setInput, bumpSnippetUse, inputRef, slashMode]);

  const pickSnippet = useCallback((s: Snippet) => {
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
  }, [applySnippetText]);

  const confirmSnippetParams = useCallback(async () => {
    if (!paramTarget) return;
    const text = fillParams(paramTarget.content, paramValues);
    await applySnippetText(text, paramTarget.id);
  }, [paramTarget, paramValues, applySnippetText]);

  /** Open picker from the toolbar button (manual mode). */
  const openManual = useCallback(() => {
    setSlashMode(false);
    setSnippetQuery("");
    setSnippetOpen((v) => !v);
  }, []);

  /** React to composer text: open/filter when a trailing `/query` is present. */
  const syncSlashFromInput = useCallback((value: string) => {
    const hit = matchSlashCommand(value);
    if (!hit) {
      if (slashMode) {
        setSlashMode(false);
        setSnippetOpen(false);
        setSnippetQuery("");
      }
      return;
    }
    setSlashMode(true);
    setSnippetOpen(true);
    setSnippetQuery(hit.query);
  }, [slashMode]);

  const effectiveQuery = slashMode
    ? (matchSlashCommand(input)?.query ?? snippetQuery)
    : snippetQuery;

  const snippetChoices = useMemo(() => {
    const q = effectiveQuery.trim().toLowerCase();
    // Prompt Studio templates (kind='prompt') are optimizer recipes, not insertable fragments
    const list = [...(snippets || [])]
      .filter((s) => s.kind !== "prompt")
      .sort((a, b) => b.useCount - a.useCount || b.updatedAt.localeCompare(a.updatedAt));
    const filtered = q
      ? list.filter(
          (s) =>
            s.name.toLowerCase().includes(q) ||
            s.tags.toLowerCase().includes(q) ||
            s.content.toLowerCase().includes(q)
        )
      : list;
    return filtered.slice(0, SNIPPET_PICK_LIMIT);
  }, [snippets, effectiveQuery]);

  return {
    snippets,
    snippetOpen,
    setSnippetOpen,
    snippetQuery: effectiveQuery,
    setSnippetQuery,
    paramTarget,
    setParamTarget,
    paramValues,
    setParamValues,
    snippetChoices,
    pickSnippet,
    confirmSnippetParams,
    openManual,
    syncSlashFromInput,
    slashMode,
  };
}
