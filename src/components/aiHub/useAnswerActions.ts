import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { AiHubTurn } from "../../core/types";
import { makeTitle } from "./config";
import { keepableAnswer } from "./keepable";
import { useTimedFlag } from "./useAiHubWindow";

/** Copy / save-as-snippet / export / quote-selection for answers in the transcript. */
export function useAnswerActions(opts: {
  history: AiHubTurn[];
  setError: (msg: string) => void;
  setInput: (updater: (prev: string) => string) => void;
  inputRef: { current: HTMLTextAreaElement | null };
  selectedText: string;
  setSelectedText: (v: string) => void;
  exportSession: (turns: AiHubTurn[]) => Promise<boolean | undefined>;
}) {
  const { t } = useTranslation("aihub");
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const { history, setError, setInput, inputRef, selectedText, setSelectedText, exportSession } = opts;

  /** Transcript index of the assistant message that was just copied / saved. */
  const [copied, setCopied] = useState<number | null>(null);
  const [snippetSaved, setSnippetSaved] = useState<number | null>(null);
  const [exportDone, flashExportDone] = useTimedFlag(1500);

  const textAt = (index: number) =>
    keepableAnswer(history[index]?.content || "", history[index]?.keep === "whole").text;

  /** `index` points at an assistant turn in the transcript. */
  const saveAnswerAsSnippet = async (index: number) => {
    const text = textAt(index);
    if (!text) return;
    const question = history[index - 1]?.content;
    const name = makeTitle(question || text, t("saveSnippet"));
    try {
      await addSnippet({
        name,
        content: text,
        tags: "aihub,ai",
        params: "",
        kind: "prompt",
        scenario: "general",
      });
      setSnippetSaved(index);
      window.setTimeout(() => setSnippetSaved((cur) => (cur === index ? null : cur)), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const copyAnswer = async (index: number) => {
    const text = textAt(index);
    if (!text) return;
    try {
      await invokeCopyToClipboard(text);
      setCopied(index);
      window.setTimeout(() => setCopied((cur) => (cur === index ? null : cur)), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleExport = async () => {
    if (await exportSession(history)) flashExportDone();
  };

  /** Quote the selected answer text into the input as a follow-up prompt. */
  const quoteSelection = () => {
    const text = selectedText.trim().slice(0, 500);
    if (!text) return;
    const quoted = t("quotePrefix", { text });
    setInput((prev) => (prev ? `${prev}\n\n${quoted}` : quoted));
    setSelectedText("");
    window.getSelection()?.removeAllRanges();
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  return { copied, snippetSaved, exportDone, saveAnswerAsSnippet, copyAnswer, handleExport, quoteSelection };
}
