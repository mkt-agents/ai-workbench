export type PromptScenario =
  | "general"
  | "translate"
  | "product"
  | "code"
  | "agent"
  | "copywriting";

export type PromptGoal =
  | "concrete"
  | "role"
  | "format"
  | "constraints"
  | "shorten"
  | "preserve";

export type PromptTemplate = {
  id: string;
  titleKey: string;
  body: string;
};

export const PROMPT_SCENARIOS: PromptScenario[] = [
  "general",
  "translate",
  "product",
  "code",
  "agent",
  "copywriting",
];

export const PROMPT_GOALS: PromptGoal[] = [
  "concrete",
  "role",
  "format",
  "constraints",
  "shorten",
  "preserve",
];

export const DEFAULT_PROMPT_GOALS: PromptGoal[] = [
  "concrete",
  "format",
  "constraints",
];

const SCENARIO_SYSTEM: Record<PromptScenario, string> = {
  general:
    "你是提示词润色专家。把用户给出的草稿改写成更清晰、可执行的提示词。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
  translate:
    "你是翻译与润色向的提示词专家。把草稿改写成适合翻译/改写任务的提示词：" +
    "明确忠实或意译边界、语气与领域、术语约束；禁止擅自增删事实。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
  product:
    "你是产品与需求向的提示词专家。把草稿改写成适合 PRD/用户故事/功能拆解的提示词：" +
    "目标可验收、角色与场景清晰、验收标准具体；禁止空泛产品黑话。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
  code:
    "你是资深工程师向的提示词润色专家。把草稿改写成适合编程/调试任务的提示词：" +
    "目标明确、技术约束清楚、可验收。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
  agent:
    "你是 Agent 系统提示词专家。把草稿改写成可用的系统提示：" +
    "角色、能力边界、工具使用原则、输出格式清晰。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
  copywriting:
    "你是内容与文案向的提示词润色专家。强化受众、语气、结构与交付物。" +
    "保留原意与语言；只输出优化后的提示词全文，不要解释、不要标题、不要 markdown 围栏。",
};

const GOAL_INSTRUCTIONS: Record<PromptGoal, string> = {
  concrete: "更具体、可执行，避免含糊指令",
  role: "补全合适的角色与必要上下文",
  format: "明确期望的输出格式与结构",
  constraints: "补充约束、边界与不应做的事",
  shorten: "在不丢关键信息的前提下缩短精炼",
  preserve: "尽量少改写，主要理顺结构与表述",
};

/** @deprecated use PROMPT_TEMPLATES */
export const PROMPT_EXAMPLE: Record<PromptScenario, string> = {
  general: "帮我写一份周报，把这周做的事说清楚。",
  translate: "把这段中文译成英文，语气正式，术语保持一致。",
  product: "根据功能点写用户故事和验收标准。",
  code: "这个 React 组件卡顿，帮我看看怎么优化。",
  agent: "你是编程助手，帮用户改代码。要小心，别乱改。有问题就问。",
  copywriting: "给新产品写个宣传文案，要有吸引力。",
};

export const PROMPT_TEMPLATES: Record<PromptScenario, PromptTemplate[]> = {
  general: [
    {
      id: "general-weekly",
      titleKey: "prompts.templates.generalWeekly",
      body: [
        "根据以下材料写一份周报 Markdown。",
        "只输出正文；无空话；未提供的信息不要编造。",
        "",
        "【本周事项】",
        "- ",
        "",
        "【进展/数据】",
        "- ",
        "",
        "【下周计划】",
        "- ",
        "",
        "【风险】（可空）",
        "- ",
        "",
        "输出结构：",
        "## 本周完成",
        "## 关键进展",
        "## 下周计划",
        "## 风险与阻塞",
      ].join("\n"),
    },
    {
      id: "general-meeting",
      titleKey: "prompts.templates.generalMeeting",
      body: [
        "根据以下要点整理一封可直接发送的会议纪要邮件正文。",
        "只输出邮件正文（含主题行）；语气专业简洁；不要 Markdown 围栏。",
        "",
        "【会议主题/时间】",
        "",
        "【要点】",
        "- ",
        "",
        "【待办与负责人】",
        "- ",
        "",
        "输出包含：主题、问候、纪要要点、待办列表（负责人+截止若有）、结尾。",
      ].join("\n"),
    },
    {
      id: "general-decide",
      titleKey: "prompts.templates.generalDecide",
      body: [
        "根据以下信息做方案对比，并给出可执行建议。",
        "只输出 Markdown；对比维度具体；结论写清推荐与理由。",
        "",
        "【目标】",
        "",
        "【方案 A】",
        "",
        "【方案 B】",
        "",
        "【约束】（成本/工期/风险等，可空）",
        "",
        "输出结构：",
        "## 对比维度",
        "## 方案对比",
        "## 建议结论",
      ].join("\n"),
    },
  ],
  translate: [
    {
      id: "translate-bilingual",
      titleKey: "prompts.templates.translateBilingual",
      body: [
        "根据以下要求完成翻译。",
        "只输出译文；不擅自增删事实；术语前后一致。",
        "",
        "【原文】",
        "",
        "【目标语言】",
        "",
        "【语气/领域】（可空）",
        "",
      ].join("\n"),
    },
    {
      id: "translate-tone",
      titleKey: "prompts.templates.translateTone",
      body: [
        "按目标语气润色原文，不改变事实与关键信息。",
        "只输出润色稿；不要解释；不要 markdown 围栏。",
        "",
        "【原文】",
        "",
        "【目标语气】（如正式/口语/对客户）",
        "",
        "【保留项】（必须保留的用词或结构，可空）",
        "- ",
        "",
      ].join("\n"),
    },
    {
      id: "translate-glossary",
      titleKey: "prompts.templates.translateGlossary",
      body: [
        "按术语表翻译原文，并给出简短术语对照。",
        "译文须遵守术语表与禁止译法；不要编造未出现的术语。",
        "",
        "【原文】",
        "",
        "【术语表】（原文=译法）",
        "- ",
        "",
        "【禁止译法】（可空）",
        "- ",
        "",
        "输出结构：",
        "## 译文",
        "## 术语对照",
      ].join("\n"),
    },
  ],
  product: [
    {
      id: "product-prd",
      titleKey: "prompts.templates.productPrd",
      body: [
        "根据以下信息整理一份需求说明 Markdown。",
        "只输出正文；条目可验收；禁止空泛口号；未提供的信息不要编造。",
        "",
        "【背景】",
        "",
        "【目标用户】",
        "",
        "【功能点】",
        "- ",
        "",
        "【非目标】（可空）",
        "- ",
        "",
        "输出结构：",
        "## 背景与目标",
        "## 目标用户",
        "## 功能说明",
        "## 非目标",
        "## 验收要点",
      ].join("\n"),
    },
    {
      id: "product-stories",
      titleKey: "prompts.templates.productStories",
      body: [
        "根据以下信息写出用户故事与验收标准。",
        "只输出 Markdown；故事用「作为…我希望…以便…」；验收用 Given/When/Then。",
        "",
        "【功能】",
        "",
        "【角色】",
        "",
        "【场景】（可空）",
        "",
        "输出结构：",
        "## 用户故事",
        "## 验收标准",
      ].join("\n"),
    },
    {
      id: "product-breakdown",
      titleKey: "prompts.templates.productBreakdown",
      body: [
        "把目标拆成可交付的功能模块，并标优先级与依赖。",
        "只输出 Markdown；拆分具体可排期；不要编造未提供的已有能力。",
        "",
        "【目标】",
        "",
        "【约束】（工期/技术/合规等，可空）",
        "",
        "【已有能力】（可空）",
        "- ",
        "",
        "输出结构：",
        "## 模块拆分",
        "## 优先级",
        "## 依赖关系",
      ].join("\n"),
    },
  ],
  code: [
    {
      id: "code-perf",
      titleKey: "prompts.templates.codePerf",
      body: [
        "根据以下现象排查性能/卡顿问题，给出可执行的排查与优化建议。",
        "只输出 Markdown；按可能性排序；不要编造未提供的栈或指标。",
        "",
        "【现象】",
        "",
        "【组件/路径】",
        "",
        "【已尝试】（可空）",
        "- ",
        "",
        "输出结构：",
        "## 可能原因",
        "## 排查步骤",
        "## 改法建议",
      ].join("\n"),
    },
    {
      id: "code-review",
      titleKey: "prompts.templates.codeReview",
      body: [
        "对以下代码做 Review，找出 bug、风险与可改进点。",
        "只输出 Markdown；按严重级别分组；指出具体位置；禁止空泛评价。",
        "",
        "【代码/路径】",
        "",
        "【上下文】（业务目的、约束，可空）",
        "",
        "输出结构：",
        "## 严重（Bug/安全）",
        "## 风险",
        "## 改进建议",
      ].join("\n"),
    },
    {
      id: "code-test",
      titleKey: "prompts.templates.codeTest",
      body: [
        "为以下函数/行为设计单元测试用例。",
        "只输出 Markdown；覆盖正常、边界、异常；可写断言要点，勿编造不存在的 API。",
        "",
        "【函数/行为】",
        "",
        "【技术栈】（如 Jest / Vitest / JUnit，可空）",
        "",
        "输出结构：",
        "## 正常路径",
        "## 边界情况",
        "## 异常情况",
        "## 断言要点",
      ].join("\n"),
    },
    {
      id: "code-qa-md",
      titleKey: "prompts.templates.codeQaMd",
      body: [
        "根据提供的开发说明，提取关键信息并生成一份结构化的 Markdown 测试交接文档。",
        "",
        "【输入解析规则】",
        "- 【背景】：提取产品/模块名、版本/分支，用于生成概述。",
        "- 【本次做了什么】：提取功能点，用于生成功能说明和推导验证场景。",
        "- 【改了哪些地方】：提取页面/模块/关键文件路径，用于生成改动范围和推导回归关注点。",
        "- 【建议重点验证】：提取已知风险点，用于生成异常与边界及风险与已知问题。",
        "- 【其它】：提取前置账号、测试数据、已知缺陷、排除范围，用于生成测试前置和不在本次范围。",
        "",
        "【输出格式与结构约束】",
        "1. 仅输出 Markdown 正文，不包含任何解释、问候或 Markdown 围栏。",
        "2. 严格遵循以下章节结构，使用 H2 (##) 和 H3 (###) 标题。",
        "3. 所有验证项必须使用 - [ ] 未勾选列表格式。",
        "4. 表述必须可执行、无空话，禁止出现“可能”、“大概”等模糊词汇。",
        "",
        "【各章节生成标准】",
        "- ## 概述：结合背景和本次做了什么，用一两句话精准说明本次交付的模块及核心变更目的。",
        "- ## 功能说明：将本次做了什么转化为列表，写清具体实现了什么功能。",
        "- ## 改动范围：将改了哪些地方转化为列表，写清受影响的模块、页面或关键文件路径（若无路径则写模块名）。",
        "- ## 测试前置：提取其它中的环境、账号、配置、依赖数据；若无则输出“无特殊前置”。",
        "- ## 验证场景：基于功能点和改动范围推导，分三组输出，均为 - [ ] 项：",
        "  - ### 正常路径：核心功能的正向验证步骤。",
        "  - ### 异常与边界：结合建议重点验证，列出异常输入、边界值及错误处理验证。",
        "  - ### 回归关注：基于改动范围，列出可能受影响的上下游模块或历史核心功能的回归验证。",
        "- ## 风险与已知问题：提取建议重点验证和其它的已知缺陷与风险；若无则输出“无”。",
        "- ## 不在本次范围：提取其它中明确排除的功能或模块；若无则输出“无”。",
        "",
        "【禁止事项】",
        "- 禁止编造输入中未提供的产品名、路径或功能。",
        "- 禁止添加任何文档外的修饰性文字。",
        "",
        "【开发说明】（在下方填写）",
        "【背景】",
        "",
        "【本次做了什么】",
        "- ",
        "",
        "【改了哪些地方】",
        "- ",
        "",
        "【建议重点验证】",
        "- ",
        "",
        "【其它】",
        "",
      ].join("\n"),
    },
  ],
  agent: [
    {
      id: "agent-coder",
      titleKey: "prompts.templates.agentCoder",
      body: [
        "根据以下要求，生成一份可直接粘贴使用的「编程助手」系统提示词全文。",
        "只输出系统提示正文；边界清晰；不要解释、不要围栏。",
        "",
        "【角色边界】",
        "",
        "【必须做】",
        "- ",
        "",
        "【禁止做】",
        "- ",
        "",
        "【输出格式】",
        "",
      ].join("\n"),
    },
    {
      id: "agent-research",
      titleKey: "prompts.templates.agentResearch",
      body: [
        "根据以下要求，生成一份可直接粘贴使用的「研究助手」系统提示词全文。",
        "只输出系统提示正文；强调有依据、不确定要标明；不要解释、不要围栏。",
        "",
        "【角色边界】",
        "",
        "【必须做】",
        "- ",
        "",
        "【禁止做】",
        "- ",
        "",
        "【输出格式】",
        "",
      ].join("\n"),
    },
    {
      id: "agent-ops",
      titleKey: "prompts.templates.agentOps",
      body: [
        "根据以下要求，生成一份可直接粘贴使用的「运维排查助手」系统提示词全文。",
        "只输出系统提示正文；先收集现象与日志再给步骤；不要解释、不要围栏。",
        "",
        "【角色边界】",
        "",
        "【必须做】",
        "- ",
        "",
        "【禁止做】",
        "- ",
        "",
        "【输出格式】",
        "",
      ].join("\n"),
    },
  ],
  copywriting: [
    {
      id: "copy-launch",
      titleKey: "prompts.templates.copyLaunch",
      body: [
        "根据以下信息写产品宣传文案。",
        "只输出文案正文；卖点具体；贴合受众与语气；不要编造未提供的数据和承诺。",
        "",
        "【产品】",
        "",
        "【卖点】",
        "- ",
        "",
        "【受众】",
        "",
        "【语气】",
        "",
      ].join("\n"),
    },
    {
      id: "copy-landing",
      titleKey: "prompts.templates.copyLanding",
      body: [
        "根据以下信息写落地页首屏文案。",
        "只输出三行：标题、副标题、行动号召（CTA）；简洁有力。",
        "",
        "【产品】",
        "",
        "【卖点】",
        "- ",
        "",
        "【CTA】（期望用户动作，可空）",
        "",
        "输出格式：",
        "标题：",
        "副标题：",
        "CTA：",
      ].join("\n"),
    },
    {
      id: "copy-social",
      titleKey: "prompts.templates.copySocial",
      body: [
        "根据以下信息写 3 条短视频口播文案。",
        "只输出 3 条；口语化；每条默认不超过 80 字（若【时长/字数】另有要求则从其规定）。",
        "",
        "【主题】",
        "",
        "【时长/字数】（可空）",
        "",
        "【卖点】",
        "- ",
        "",
        "输出格式：",
        "1. …",
        "2. …",
        "3. …",
      ].join("\n"),
    },
  ],
};

export function buildPromptOptimizeMessages(
  scenario: PromptScenario,
  goals: PromptGoal[],
  draft: string
): { system: string; user: string } {
  const selected = goals.length > 0 ? goals : DEFAULT_PROMPT_GOALS;
  const goalLines = selected.map((g) => `- ${GOAL_INSTRUCTIONS[g]}`).join("\n");
  const user = [
    "请按以下目标润色提示词：",
    goalLines,
    "",
    "原稿：",
    draft.trim(),
    "",
    "只输出优化后的提示词全文。",
  ].join("\n");

  return { system: SCENARIO_SYSTEM[scenario], user };
}

export function buildExplainChangesMessages(
  original: string,
  polished: string
): { system: string; user: string } {
  return {
    system:
      "你是提示词编辑说明助手。对比「原稿」与「润色结果」，用 3～5 条中文要点说明改了什么、为何更好。" +
      "只输出要点列表（每条一行，可用 - 开头），不要复述全文，不要 markdown 围栏。",
    user: ["原稿：", original.trim(), "", "润色结果：", polished.trim()].join("\n"),
  };
}

export function cleanOptimizedPrompt(raw: string): string {
  let s = raw.trim();
  if (s.startsWith("```")) {
    s = s.replace(/^```[a-zA-Z]*\n?/, "").replace(/\n?```\s*$/, "").trim();
  }
  s = s.replace(/^["'「『]+/, "").replace(/["'」』]+$/, "").trim();
  return s;
}

export type PromptHistoryItem = {
  id: string;
  at: number;
  scenario: PromptScenario;
  goals: PromptGoal[];
  input: string;
  output: string;
  pinned?: boolean;
};

const HISTORY_KEY = "ai-workbench.prompt-studio.history";
const HISTORY_LIMIT = 50;
const HISTORY_EXPORT_VERSION = 1;
const CUSTOM_TEMPLATES_KEY = "ai-workbench.prompt-studio.custom-templates";

function sortHistory(items: PromptHistoryItem[]): PromptHistoryItem[] {
  return [...items].sort((a, b) => {
    const pin = Number(!!b.pinned) - Number(!!a.pinned);
    if (pin !== 0) return pin;
    return b.at - a.at;
  });
}

function isPromptScenario(value: unknown): value is PromptScenario {
  return (
    typeof value === "string" &&
    (PROMPT_SCENARIOS as string[]).includes(value)
  );
}

function normalizeHistoryItem(raw: unknown): PromptHistoryItem | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.id !== "string" || !o.id) return null;
  if (typeof o.at !== "number" || !Number.isFinite(o.at)) return null;
  if (!isPromptScenario(o.scenario)) return null;
  if (typeof o.input !== "string" || typeof o.output !== "string") return null;
  const goals = Array.isArray(o.goals)
    ? (o.goals.filter((g) => typeof g === "string") as PromptGoal[])
    : [];
  return {
    id: o.id,
    at: o.at,
    scenario: o.scenario,
    goals,
    input: o.input,
    output: o.output,
    pinned: !!o.pinned,
  };
}

export function loadPromptHistory(): PromptHistoryItem[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const items = parsed
      .map(normalizeHistoryItem)
      .filter((x): x is PromptHistoryItem => !!x);
    return sortHistory(items).slice(0, HISTORY_LIMIT);
  } catch {
    return [];
  }
}

export function savePromptHistory(items: PromptHistoryItem[]): void {
  try {
    localStorage.setItem(
      HISTORY_KEY,
      JSON.stringify(sortHistory(items).slice(0, HISTORY_LIMIT))
    );
  } catch {
    /* ignore quota */
  }
}

export function pushPromptHistory(
  prev: PromptHistoryItem[],
  item: Omit<PromptHistoryItem, "id" | "at">
): PromptHistoryItem[] {
  const next = sortHistory([
    {
      ...item,
      id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      at: Date.now(),
      pinned: false,
    },
    ...prev,
  ]).slice(0, HISTORY_LIMIT);
  savePromptHistory(next);
  return next;
}

export function toggleHistoryPinned(
  prev: PromptHistoryItem[],
  id: string
): PromptHistoryItem[] {
  const next = sortHistory(
    prev.map((h) => (h.id === id ? { ...h, pinned: !h.pinned } : h))
  );
  savePromptHistory(next);
  return next;
}

export function exportPromptHistory(items: PromptHistoryItem[]): string {
  return JSON.stringify(
    {
      version: HISTORY_EXPORT_VERSION,
      exportedAt: Date.now(),
      items: sortHistory(items),
    },
    null,
    2
  );
}

export function importPromptHistory(
  prev: PromptHistoryItem[],
  json: string
): PromptHistoryItem[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    throw new Error("invalid_json");
  }

  let list: unknown[] = [];
  if (Array.isArray(parsed)) {
    list = parsed;
  } else if (parsed && typeof parsed === "object") {
    const o = parsed as Record<string, unknown>;
    if (Array.isArray(o.items)) list = o.items;
    else throw new Error("invalid_shape");
  } else {
    throw new Error("invalid_shape");
  }

  const incoming = list
    .map(normalizeHistoryItem)
    .filter((x): x is PromptHistoryItem => !!x);
  if (incoming.length === 0) throw new Error("empty");

  const byId = new Map<string, PromptHistoryItem>();
  for (const item of prev) byId.set(item.id, item);
  for (const item of incoming) byId.set(item.id, item);

  const next = sortHistory([...byId.values()]).slice(0, HISTORY_LIMIT);
  savePromptHistory(next);
  return next;
}

export type CustomPromptTemplate = {
  id: string;
  scenario: PromptScenario;
  title: string;
  body: string;
  at: number;
};

function normalizeCustomTemplate(raw: unknown): CustomPromptTemplate | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.id !== "string" || !o.id) return null;
  if (!isPromptScenario(o.scenario)) return null;
  if (typeof o.title !== "string" || !o.title.trim()) return null;
  if (typeof o.body !== "string") return null;
  const at = typeof o.at === "number" && Number.isFinite(o.at) ? o.at : Date.now();
  return {
    id: o.id,
    scenario: o.scenario,
    title: o.title.trim(),
    body: o.body,
    at,
  };
}

export function loadCustomTemplates(): CustomPromptTemplate[] {
  try {
    const raw = localStorage.getItem(CUSTOM_TEMPLATES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map(normalizeCustomTemplate)
      .filter((x): x is CustomPromptTemplate => !!x)
      .sort((a, b) => b.at - a.at);
  } catch {
    return [];
  }
}

/**
 * One-shot migration helper: custom templates now live in the snippets table
 * (kind = 'prompt'). Returns whatever is still in localStorage and clears the
 * key, so the store can merge them into SQLite on next load.
 */
export function takeLegacyCustomTemplates(): CustomPromptTemplate[] {
  const items = loadCustomTemplates();
  if (items.length > 0) {
    try {
      localStorage.removeItem(CUSTOM_TEMPLATES_KEY);
    } catch {
      /* ignore */
    }
  }
  return items;
}
