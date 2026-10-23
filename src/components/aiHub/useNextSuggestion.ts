import { useCallback, useEffect, useRef, useState } from "react";
import type { AIModelConfig, AiHubTurn } from "../../core/types";
import { recordAiUsage } from "../../lib/aiUsage";

type GenerateText = (req: {
  config: AIModelConfig;
  system: string;
  user: string;
}) => Promise<string>;

const SUGGEST_SYSTEM = [
  "你是「天玑」AI 助手的输入建议器，根据对话预测用户下一步最可能发出的指令。",
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
const CJK_ALL_RE = /[\u4e00-\u9fff\u3400-\u4dbf]/g;
const LATIN_ALL_RE = /[A-Za-z]/g;
/** 助手口吻，或推理模型在复述「用户问了什么」——都不是用户会发的指令 */
const META_OPENING_RE =
  /^(let me|i'?ll|i will|i need|i should|the user|user (asked|wants|is)|okay|ok,|so,|我来|让我|我先|用户(问|想|在|刚|要|希望))/i;
/** 提示词要求 ≤24 字；明显更长的多半是思考过程被当成了正文 */
const MAX_SUGGESTION_CHARS = 60;

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
  if (line.length < 2 || line.length > MAX_SUGGESTION_CHARS) return null;
  // 助手口吻的半截话（Let me ...:）、复述用户意图的思考过程，都不是用户指令
  if (META_OPENING_RE.test(line) || /[:：]$/.test(line)) return null;
  // 用户在说中文，建议却以外语为主 → 语言不符。只看「有没有中文」不够：英文思考里
  // 常引用用户的中文原话。允许夹带文件名、函数名（拉丁字母不超过中文的两倍）
  if (expectCJK) {
    const cjk = (line.match(CJK_ALL_RE) || []).length;
    const latin = (line.match(LATIN_ALL_RE) || []).length;
    if (cjk === 0 || latin > cjk * 2) return null;
  }
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
  turns: AiHubTurn[];
  busy: boolean;
  /** Off = no extra model call per round (the chip is a nicety, not a requirement). */
  enabled: boolean;
  generate: GenerateText;
  fallback?: () => string | null;
}) {
  const { model, turns, busy, enabled, generate, fallback } = opts;
  const [suggestion, setSuggestion] = useState<string | null>(null);
  const genRef = useRef(0);
  /** 唤出窗口会重载模型列表，model 对象换了 identity 但内容没变——
      effect 只认 model.id，对象本身走 ref，避免每次唤出都白打一次请求。 */
  const modelRef = useRef(model);
  modelRef.current = model;
  const generateRef = useRef(generate);
  generateRef.current = generate;
  const fallbackRef = useRef(fallback);
  fallbackRef.current = fallback;
  /** 上次已发起预测的对话快照；同一快照不重复请求，也不清掉已有建议。 */
  const lastKeyRef = useRef<string | null>(null);
  const modelId = model?.id;

  useEffect(() => {
    const model = modelRef.current;
    if (!enabled || busy || turns.length === 0) {
      ++genRef.current;
      lastKeyRef.current = null;
      setSuggestion(null);
      return;
    }
    const last = turns[turns.length - 1];
    const key = `${modelId ?? ""}|${turns.length}|${last?.content.length ?? 0}`;
    if (key === lastKeyRef.current) return;
    lastKeyRef.current = key;
    const gen = ++genRef.current;
    if (!model) {
      // 没模型跑不了预测，但关键词兜底照样有意义
      setSuggestion(fallbackRef.current?.() ?? null);
      return;
    }
    let started = false;
    const timer = window.setTimeout(() => {
      started = true;
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
          const text = await generateRef.current({
            // 推理模型先花 token 思考：给太少会在正文出来前耗尽，只剩思考过程
            config: { ...model, maxTokens: 1024, temperature: 0.4 },
            system: SUGGEST_SYSTEM,
            user: transcript,
          });
          if (genRef.current !== gen) return;
          const clean = sanitizeSuggestion(text, expectCJK);
          // This call is invisible to the user, so it must still land in the ledger.
          recordAiUsage({
            modelId: model.id,
            modelName: model.name,
            estimatedTokens: Math.max(1, Math.round((transcript.length + text.length) / 4)),
          });
          if (clean) setSuggestion(clean);
          else {
            console.warn("[qa-suggest] 建议内容不合格，改用本地兜底:", text.slice(0, 120));
            setSuggestion(fallbackRef.current?.() ?? null);
          }
        } catch (e) {
          if (genRef.current === gen) {
            console.warn("[qa-suggest] 下一步提示词生成失败，改用本地兜底:", e);
            setSuggestion(fallbackRef.current?.() ?? null);
          }
        }
      })();
    }, 900);
    return () => {
      window.clearTimeout(timer);
      // 计时器没跑就被清掉时，这个快照还没真正请求过，下次要重新发
      if (genRef.current === gen && !started) lastKeyRef.current = null;
    };
  }, [turns, busy, enabled, modelId]);

  const dismiss = useCallback(() => setSuggestion(null), []);
  return { suggestion, dismiss };
}
