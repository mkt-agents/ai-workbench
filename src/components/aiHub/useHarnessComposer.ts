import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useGlobalStore } from "../../core/store";
import type { Snippet } from "../../core/types";
import { applyCompletion, atQuery, matchesQuery, type PaletteItem } from "./harnessCommands";

/** 内置 / 项目命令：id 用于过滤，label 与 hint 已翻译好，run 执行真正的动作。
 * 带 `options` 的命令不直接执行，而是在同一个浮层里展开二级列表让用户挑一项。 */
export type HarnessCommand = {
  id: string;
  label: string;
  hint: string;
  run?: () => void;
  options?: PaletteItem[];
};

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
  /** 二级列表（`/mode` 这类「选一个」的命令）；null = 还在第一层。 */
  const [drill, setDrill] = useState<{ title: string; items: PaletteItem[] } | null>(null);
  /** 打开二级列表那一刻的输入内容：输入一变就说明用户在打正文了，列表该让位。 */
  const drillBaseRef = useRef("");
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

  /** 二级列表是否该占着浮层：输入一动（开始打正文）就让位给正常输入。 */
  const drilling = drill !== null && input === drillBaseRef.current;

  const items = useMemo<PaletteItem[] | null>(() => {
    // 二级列表优先：`/mode` 点开后整个浮层换成清单，直到选完或退出
    if (drill && drilling) return drill.items;
    if (slashOpen) {
      const q = slashQueryText;
      const out: PaletteItem[] = [];
      for (const command of commands) {
        if (!matchesQuery(command.id, q) && !matchesQuery(command.label, q)) continue;
        out.push({
          kind: "command",
          id: command.id,
          label: command.label,
          hint: command.hint,
          drill: Boolean(command.options?.length),
        });
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
  }, [drill, drilling, slashOpen, slashQueryText, commands, skills, snippets, fileQuery, files]);

  /** 列表换了（更窄的查询）就回到第一项，别让高亮停在越界位置。 */
  useEffect(() => {
    // 二级列表打开时高亮由 pick 定（当前值），别被查询变化抢回去
    if (drilling) return;
    setActive(0);
  }, [slashQueryText, fileQuery, drilling]);

  useEffect(() => {
    setActive((cur) => (items && cur < items.length ? cur : 0));
  }, [items]);

  /** 二级列表开着时用户开始打正文（或重新输入 `/`）：直接收起，别挡着输入。 */
  useEffect(() => {
    if (drill && !drilling) setDrill(null);
  }, [drill, drilling]);

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
        // 先把 `/xxx` 从输入里摘掉，再执行命令自带的动作（或展开二级列表）
        const stripped = atQuery(input) !== null
          ? input
          : input.replace(/(?:^|\s)\/[^\s/]*$/, "").trimEnd();
        setInput((prev) => {
          const hit = atQuery(prev);
          if (hit !== null) return prev;
          return prev.replace(/(?:^|\s)\/[^\s/]*$/, "").trimEnd();
        });
        if (command?.options?.length) {
          // 高亮当前值：直接回车也不会莫名改掉模式
          const current = command.options.findIndex((o) => o.kind === "option" && o.checked);
          drillBaseRef.current = stripped;
          setActive(current >= 0 ? current : 0);
          setDrill({ title: command.label, items: command.options });
          return;
        }
        command?.run?.();
        return;
      }
      if (item.kind === "option") {
        item.run();
        setDrill(null);
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
    [commands, snippets, input, onPickSnippet, onRef, setInput],
  );

  const pickActive = useCallback(() => {
    const item = items?.[active] ?? items?.[0];
    if (item) pick(item);
  }, [items, active, pick]);

  const closeDrill = useCallback(() => setDrill(null), []);

  return {
    items,
    active,
    setActive,
    move,
    pick,
    pickActive,
    /** 二级列表标题（如「/mode 切换任务模式」）；null = 不在二级列表。 */
    drillTitle: drilling ? (drill?.title ?? null) : null,
    closeDrill,
  };
}
