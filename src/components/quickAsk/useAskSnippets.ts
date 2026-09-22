import { useCallback, useMemo, useState } from "react";
import { useGlobalStore } from "../../core/store";
import type { Snippet } from "../../core/types";
import { fillParams, parseParamNames } from "../../lib/snippets";
import { SNIPPET_PICK_LIMIT } from "./config";

export type AskSnippetsOptions = {
  setInput: (updater: string | ((prev: string) => string)) => void;
  inputRef: { current: HTMLTextAreaElement | null };
};

/** Snippet picker: search, param-fill modal, insert into the input box. */
export function useAskSnippets(opts: AskSnippetsOptions) {
  const { setInput, inputRef } = opts;
  const snippets = useGlobalStore((s) => s.snippets);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);

  const [snippetOpen, setSnippetOpen] = useState(false);
  const [snippetQuery, setSnippetQuery] = useState("");
  const [paramTarget, setParamTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});

  const applySnippetText = useCallback(async (text: string, snippetId: string) => {
    setInput((prev) => (prev ? `${prev}\n${text}` : text));
    setSnippetOpen(false);
    setParamTarget(null);
    setSnippetQuery("");
    await bumpSnippetUse(snippetId);
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [setInput, bumpSnippetUse, inputRef]);

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

  return {
    snippets,
    snippetOpen,
    setSnippetOpen,
    snippetQuery,
    setSnippetQuery,
    paramTarget,
    setParamTarget,
    paramValues,
    setParamValues,
    snippetChoices,
    pickSnippet,
    confirmSnippetParams,
  };
}
