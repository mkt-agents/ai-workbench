import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useGlobalStore } from "../../core/store";
import type { Snippet } from "../../core/types";
import { applyCompletion, atQuery, matchesQuery, type PaletteItem } from "./harnessCommands";

/** 内置 / 项目命令：id 用于过滤，label 与 hint 已翻译好，run 执行真正的动作。 */
export type HarnessCommand = { id: string; label: string; hint: string; run: () => void };

export type HarnessSkillItem = {
  name: string;
  description: string;
  whenToUse?: string;
  scope: string;
  allowedTools: string[];
};

type Options = {
  input: string;
  setInput: (updater: string | ((prev: string) => string)) => void;
  /** 工作区根目录；空 = 不提供 `@` 文件补全。 */
  root: string;
  skills: HarnessSkillItem[];
  commands: HarnessCommand[];
  /** 现有片段（斜杠模式时与命令一起列出来）。 */
  snippets: Snippet[];
  /** 正在输入 `/query`（由 useAskSnippets 判定）。 */
  slashOpen: boolean;
  slashQueryText: string;
  onPickSnippet: (snippet: Snippet) => void;
  onRef: (path: string) => void;
};

const FILE_LIMIT = 8;
const SNIPPET_LIMIT = 6;

/**
 * 输入语法层的状态机：`/` 走命令面板（内置命令 + 技能 + 片段），`@` 走文件补全。
 * 真正执行什么由调用方注入，这里只管匹配、高亮与插入。
 */
export function useHarnessComposer(opts: Options) {
  const {
    input, setInput, root, skills, commands, snippets,
    slashOpen, slashQueryText, onPickSnippet, onRef,
  } = opts;
  const invokeListFiles = useGlobalStore((s) => s.invokeHarnessListFiles);
  const [files, setFiles] = useState<string[]>([]);
  const [active, setActive] = useState(0);
  const seqRef = useRef(0);

  const fileQuery = slashOpen ? null : atQuery(input);

  useEffect(() => {
    if (fileQuery === null || !root) {
      setFiles([]);
      return;
    }
    const seq = ++seqRef.current;
    const timer = window.setTimeout(() => {
      void invokeListFiles(root, fileQuery)
        .then((list) => {
          if (seq !== seqRef.current) return;
          setFiles(list.slice(0, FILE_LIMIT));
        })
        .catch(() => {
          if (seq === seqRef.current) setFiles([]);
        });
    }, 250);
    return () => window.clearTimeout(timer);
  }, [fileQuery, root, invokeListFiles]);

  const items = useMemo<PaletteItem[] | null>(() => {
    if (slashOpen) {
      const q = slashQueryText;
      const out: PaletteItem[] = [];
      for (const command of commands) {
        if (!matchesQuery(command.id, q) && !matchesQuery(command.label, q)) continue;
        out.push({ kind: "command", id: command.id, label: command.label, hint: command.hint });
      }
      for (const skill of skills) {
        if (!matchesQuery(skill.name, q)) continue;
        out.push({
          kind: "skill",
          id: skill.name,
          name: skill.name,
          label: `/skill:${skill.name}`,
          hint: skill.whenToUse || skill.description,
          source: skill.scope,
          allowedTools: skill.allowedTools,
        });
      }
      for (const snippet of snippets) {
        if (out.length >= FILE_LIMIT + SNIPPET_LIMIT) break;
        // 片段的标签也参与匹配：原来片段面板就是这么过滤的
        if (!matchesQuery(snippet.name, q) && !matchesQuery(snippet.tags, q)) continue;
        out.push({ kind: "snippet", id: snippet.id, label: snippet.name, hint: snippet.tags });
      }
      return out;
    }
    if (fileQuery !== null && files.length > 0) {
      return files.map((path) => ({
        kind: "file" as const,
        id: path,
        label: path.split("/").pop() || path,
        hint: path,
        path,
      }));
    }
    return null;
  }, [slashOpen, slashQueryText, commands, skills, snippets, fileQuery, files]);

  /** 列表换了（更窄的查询）就回到第一项，别让高亮停在越界位置。 */
  useEffect(() => {
    setActive(0);
  }, [slashQueryText, fileQuery]);

  useEffect(() => {
    setActive((cur) => (items && cur < items.length ? cur : 0));
  }, [items]);

  const move = useCallback(
    (delta: number) => {
      const count = items?.length ?? 0;
      if (count === 0) return;
      setActive((cur) => (cur + delta + count) % count);
    },
    [items],
  );

  const pick = useCallback(
    (item: PaletteItem) => {
      if (item.kind === "command") {
        const command = commands.find((c) => c.id === item.id);
        // 先把 `/xxx` 从输入里摘掉，再执行命令自带的动作
        setInput((prev) => {
          const hit = atQuery(prev);
          if (hit !== null) return prev;
          return prev.replace(/(?:^|\s)\/[^\s/]*$/, "").trimEnd();
        });
        command?.run();
        return;
      }
      if (item.kind === "skill") {
        setInput((prev) => applyCompletion(prev, "slash", `/skill:${item.name}`));
        return;
      }
      if (item.kind === "snippet") {
        const snippet = snippets.find((s) => s.id === item.id);
        if (snippet) onPickSnippet(snippet);
        return;
      }
      // 文件选中后变成附件 chip：输入框里的 `@query` 摘掉，免得模型看到重复内容
      setInput((prev) => applyCompletion(prev, "at", "").trimEnd());
      onRef(item.path);
    },
    [commands, snippets, onPickSnippet, onRef, setInput],
  );

  const pickActive = useCallback(() => {
    const item = items?.[active] ?? items?.[0];
    if (item) pick(item);
  }, [items, active, pick]);

  return { items, active, setActive, move, pick, pickActive };
}
