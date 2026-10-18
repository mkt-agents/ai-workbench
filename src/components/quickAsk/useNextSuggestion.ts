import { useCallback, useEffect, useRef, useState } from "react";
import type { AIModelConfig, QuickAskTurn } from "../../core/types";

type GenerateText = (req: {
  config: AIModelConfig;
  system: string;
  user: string;
}) => Promise<string>;

const SUGGEST_SYSTEM = [
  "你是「快问」AI 助手的输入建议器，根据对话预测用户下一步最可能发出的指令。",
  "背景：助手能回答代码/仓库问题；代理模式下还能读写文件、执行 git（提交、推送、查状态）。",
  "规则：",
  "1. 只输出一条指令：不超过 24 个字、具体可执行的祈使句，无引号无结尾标点。",
  "2. 默认用简体中文；仅当用户最近一条消息是外语时才用那种语言。",
  "3. 优先让任务往前推进——提交/推送改动、修复发现的问题、补测试、继续没做完的步骤。",
  "4. 不要建议「查看/列出/解释」刚刚回答里已经展示过的内容，那对用户没有新信息。",
  "5. 建议要落在对话里的具体对象上（提到的文件、报错、数字），不要泛泛而谈。",
  "6. 输出是一条用户会说的话，不是助手的回答：禁止 Let me / 我来 开头，禁止以冒号结尾，",
  "   绝不允许工具调用语法、代码块或命令行（哪怕下一步要跑命令，也写成用户会说的话）。",
  "示例风格：「提交并推送这些改动」「排除临时文件后提交」「把警告也修掉」「为这个函数补个单测」",
].join("\n");

const CJK_RE = /[\u4e00-\u9fff\u3400-\u4dbf]/;
const META_OPENING_RE = /^(let me|i'?ll|i will|我来|让我|我先)/i;

/** 历史与输出里可能混入 <|tool_call_start|>、[bash(...)] 之类的工具标记，喂给模型会带偏它 */
function stripToolNoise(text: string): string {
  return text
    .replace(/<\|[^|]*\|>/g, "")
    .replace(/\[(?:bash|sh|shell|tool|command)[^\]]*\]/gi, "")
    .replace(/tool[_-]?call[a-z_]*/gi, "");
}

/** 建议必须是给人看的短指令；带代码/标记味的直接丢弃——宁可不显示，也不能显示垃圾 */
function sanitizeSuggestion(raw: string, expectCJK: boolean): string | null {
  // 先查原始输出有没有工具调用味（strip 之后特征就消失了）
  if (/tool[_-]?call|command\s*=|<\|/.test(raw)) return null;
  const cleaned = stripToolNoise(raw.trim())
    .replace(/^["'「『〈《\s]+/, "")
    .replace(/["'」』〉》.。!！?？,，、;；\s]+$/, "");
  if (!cleaned) return null;
  const line = cleaned.split("\n")[0].trim();
  if (line.length < 2) return null;
  // 助手口吻的半截话（Let me ...:）不是用户指令
  if (META_OPENING_RE.test(line) || /[:：]$/.test(line)) return null;
  // 用户在说中文，建议却是纯外语 → 语言不符
  if (expectCJK && !CJK_RE.test(line)) return null;
  // 命令调用味（bash(...)）、括号/标记碎片一律拒收——建议是句自然语言，不该长这样
  if (/\([^)]*\)|（[^）]*）|[{}<>|`]|\[\/?/.test(line)) return null;
  return line.slice(0, 48);
}

/**
 * 每轮回答结束后自动预测「下一步提示词」。错误与竞争一律静默：
 * 建议是锦上添花，任何失败都不该打扰用户。
 * `fallback`：LLM 输出不合格/失败时的确定性候选（本地关键词规则），保证常用场景稳定出建议。
 */
export function useNextSuggestion(opts: {
  model: AIModelConfig | undefined;
  turns: QuickAskTurn[];
  busy: boolean;
  generate: GenerateText;
  fallback?: () => string | null;
}) {
  const { model, turns, busy, generate, fallback } = opts;
  const [suggestion, setSuggestion] = useState<string | null>(null);
  const genRef = useRef(0);

  useEffect(() => {
    const gen = ++genRef.current;
    if (busy || turns.length === 0) {
      setSuggestion(null);
      return;
    }
    if (!model) {
      // 没模型跑不了预测，但关键词兜底照样有意义
      setSuggestion(fallback?.() ?? null);
      return;
    }
    const timer = window.setTimeout(() => {
      void (async () => {
        const lastUser = [...turns].reverse().find((t) => t.role === "user");
        const expectCJK = CJK_RE.test(lastUser?.content || "");
        const transcript = turns
          .slice(-4)
          .map(
            (t) =>
              `${t.role === "user" ? "用户" : "助手"}: ${
                (t.digest ? "[较早对话已压缩] " : "") + stripToolNoise(t.content).slice(0, 600)
              }`,
          )
          .join("\n");
        try {
          const text = await generate({
            config: { ...model, maxTokens: 384, temperature: 0.4 },
            system: SUGGEST_SYSTEM,
            user: transcript,
          });
          if (genRef.current !== gen) return;
          const clean = sanitizeSuggestion(text, expectCJK);
          if (clean) setSuggestion(clean);
          else {
            console.warn("[qa-suggest] 建议内容不合格，改用本地兜底:", text.slice(0, 120));
            setSuggestion(fallback?.() ?? null);
          }
        } catch (e) {
          if (genRef.current === gen) {
            console.warn("[qa-suggest] 下一步提示词生成失败，改用本地兜底:", e);
            setSuggestion(fallback?.() ?? null);
          }
        }
      })();
    }, 900);
    return () => window.clearTimeout(timer);
  }, [turns, busy, model, generate, fallback]);

  const dismiss = useCallback(() => setSuggestion(null), []);
  return { suggestion, dismiss };
}
