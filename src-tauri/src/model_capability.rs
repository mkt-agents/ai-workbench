//! 模型能力：模态启发式、思考协议支持和附官方来源的精确预算目录。
//!
//! 为什么是纯函数：模型广场「添加模型」时要在**不联网**的前提下先给出一个靠谱的
//! 默认值（文本 / 图文），用户可再手动覆盖。规则全部内联在这里，便于 `cargo test`
//! 钉住契约——新厂商上线只需在关键词表里补一行并加一条用例。
//!
//! 判定口径：只要命中 vision 关键词就判 `Vision`，命中 text-only 否决词则强制 `Text`，
//! 两者都没命中回落 `Unknown`（前端把 Unknown 当作「文本」默认，但标注为未确定，
//! 鼓励用户点「检测能力」做联网探测）。否决词优先级高于 vision 词：例如
//! `deepseek-vl` 要判图文，但 `deepseek-chat` 必须是纯文本，即便某天名字里混进噪声。

use serde::{Deserialize, Serialize};

pub const MIN_CONTEXT_WINDOW: usize = 4_096;
pub const MAX_CAPABILITY_TOKENS: usize = 2_000_000;

/// Only explicit provider metadata belongs here, bound to the exact connection.
/// Unknown JSON fields (including credentials) are never retained.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityCache {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
}

pub fn valid_context_window(tokens: usize) -> bool {
    (MIN_CONTEXT_WINDOW..=MAX_CAPABILITY_TOKENS).contains(&tokens)
}

fn normalized_base_url(base: &str) -> &str {
    base.trim().trim_end_matches('/')
}

impl ModelCapabilityCache {
    pub fn validated_for(&self, provider: &str, base_url: &str, model: &str) -> Option<Self> {
        let provider = provider.trim().to_ascii_lowercase();
        let base_url = normalized_base_url(base_url);
        let model = model.trim();
        if provider.is_empty() || base_url.is_empty() || model.is_empty()
            || self.provider.trim().to_ascii_lowercase() != provider
            || normalized_base_url(&self.base_url) != base_url
            || self.model.trim() != model
        {
            return None;
        }
        let context_window_tokens = self.context_window_tokens.filter(|n| valid_context_window(*n));
        let max_output_tokens = self.max_output_tokens.filter(|n| (1..=MAX_CAPABILITY_TOKENS).contains(n));
        if context_window_tokens.is_none() && max_output_tokens.is_none() {
            return None;
        }
        Some(Self { provider, base_url: base_url.into(), model: model.into(), context_window_tokens, max_output_tokens })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CatalogCapability {
    pub context_window_tokens: usize,
    pub max_output_tokens: usize,
    pub reference: &'static str,
}

/// Exact published model IDs only. No provider-wide guesses, suffix stripping,
/// substring matching or gateway aliases. Reviewed against these official specs.
pub fn catalog_capability(model: &str) -> Option<CatalogCapability> {
    let (context_window_tokens, max_output_tokens, reference) = match model.trim() {
        "deepseek-v4-flash" => (1_000_000, 384_000, "https://api-docs.deepseek.com/quick_start/pricing/"),
        "gpt-4o" | "gpt-4o-2024-08-06" | "gpt-4o-2024-11-20" =>
            (128_000, 16_384, "https://developers.openai.com/api/docs/models/gpt-4o"),
        "gpt-4o-mini" | "gpt-4o-mini-2024-07-18" =>
            (128_000, 16_384, "https://developers.openai.com/api/docs/models/gpt-4o-mini"),
        "gpt-4.1" | "gpt-4.1-2025-04-14" =>
            (1_047_576, 32_768, "https://developers.openai.com/api/docs/models/gpt-4.1"),
        "gpt-5" | "gpt-5-2025-08-07" =>
            (400_000, 128_000, "https://developers.openai.com/api/docs/models/gpt-5"),
        "o3" | "o3-2025-04-16" =>
            (200_000, 100_000, "https://developers.openai.com/api/docs/models/o3"),
        "claude-sonnet-4-20250514" =>
            (200_000, 64_000, "https://platform.claude.com/docs/en/about-claude/models/overview"),
        "claude-opus-4-20250514" =>
            (200_000, 32_000, "https://platform.claude.com/docs/en/about-claude/models/overview"),
        _ => return None,
    };
    Some(CatalogCapability { context_window_tokens, max_output_tokens, reference })
}

/// 识别结果。与前端 `visionMode` 的三态不同：这里是「推断」，前端把它映射成默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    /// 明确是多模态（可看图）。
    Vision,
    /// 明确是纯文本。
    Text,
    /// 规则库无法判断，交给用户 / 联网探测。
    Unknown,
}

impl Modality {
    pub fn as_str(self) -> &'static str {
        match self {
            Modality::Vision => "vision",
            Modality::Text => "text",
            Modality::Unknown => "unknown",
        }
    }
}

/// 强制纯文本的否决词（命中即 Text，优先于 vision 词）。
/// 这些是「名字里带 vision 噪声但实际不看图」或「该系列的纯文本成员」。
const TEXT_ONLY_MARKERS: &[&str] = &[
    "deepseek-chat",
    "deepseek-coder",
    "deepseek-r1",
    "deepseek-reasoner",
    "deepseek-v3",
    "deepseek-v2",
    "text-embedding",
    "embedding",
    "embed-",
    "bge-",
    "reranker",
    "rerank",
    "whisper",
    "tts-",
    "-tts",
    "text-moderation",
    "moderation",
    "qwen-turbo",
    "qwen-plus",
    "qwen-max",
    "qwen2.5-coder",
    "qwen3-coder",
    "qwq-",
    "o1-mini",
    "o3-mini",
    "codestral",
];

/// 多模态关键词（子串匹配，全部小写比较）。
/// 只要不被否决词挡住，命中任意一个即判 Vision。
const VISION_MARKERS: &[&str] = &[
    // OpenAI
    "gpt-4o",
    "gpt-4.1",
    "gpt-4-vision",
    "gpt-4-turbo",
    "gpt-5",
    "o1",
    "o3",
    "o4",
    "chatgpt-4o",
    "omni",
    // Anthropic（claude 3 起全系多模态）
    "claude-3",
    "claude-sonnet-4",
    "claude-opus-4",
    "claude-haiku-4",
    "claude-4",
    "claude-3.5",
    "claude-3-5",
    "claude-3.7",
    "claude-3-7",
    // Google
    "gemini",
    "gemma-3",
    // 通用 vision 标记
    "vision",
    "-vl",
    "-vl-",
    "vl-",
    "vl2",
    "llava",
    "pixtral",
    "internvl",
    "minicpm-v",
    "cogvlm",
    "glm-4v",
    "glm-4.1v",
    "glm-4.5v",
    "yi-vision",
    "yi-vl",
    "step-1v",
    "step-1o",
    // 阿里
    "qwen-vl",
    "qwen2-vl",
    "qwen2.5-vl",
    "qwen3-vl",
    "qvq",
    // Meta
    "llama-3.2",
    "llama3.2",
    "llama-4",
    "llama4",
    // Mistral
    "mistral-small-3",
    "mistral-medium-3",
    // 其它
    "molmo",
    "phi-3-vision",
    "phi-3.5-vision",
    "phi-4-multimodal",
    "aria",
    "ovis",
    "dots.vlm",
];

/// 根据厂商与模型 id 推断模态。`provider` 目前仅用于少数全系多模态厂商的兜底。
pub fn infer_modality(provider: &str, model_id: &str) -> Modality {
    let id = model_id.trim().to_lowercase();
    if id.is_empty() {
        return Modality::Unknown;
    }

    // 1) 否决词优先：embedding / 纯文本成员一律 Text。
    if TEXT_ONLY_MARKERS.iter().any(|m| id.contains(m)) {
        return Modality::Text;
    }

    // 2) vision 关键词命中即 Vision。
    if VISION_MARKERS.iter().any(|m| id.contains(m)) {
        return Modality::Vision;
    }

    // 3) 厂商兜底：个别厂商「列得出的都是多模态」，但只在没有任何否决信号时才生效。
    let p = provider.trim().to_lowercase();
    match p.as_str() {
        // Anthropic 现网全系（claude-3 起）看图；即便 id 没带版本号也按多模态兜底。
        "anthropic" => Modality::Vision,
        _ => Modality::Unknown,
    }
}

/// 便捷封装：是否应默认勾选「支持看图」。Unknown 视为否（保守，不误开 computer-use）。
pub fn is_vision_default(provider: &str, model_id: &str) -> bool {
    infer_modality(provider, model_id) == Modality::Vision
}

/// 思考强度落到哪条协议参数上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingKind {
    /// Anthropic extended thinking：`thinking.budget_tokens`。
    AnthropicBudget,
    /// OpenAI o 系 / gpt-5：`reasoning_effort`。
    OpenAiEffort,
}

/// 该模型是否支持可调的思考强度。只认能确定吃这两条参数的两类：
/// Anthropic 官方，以及 OpenAI 官方的 o1/o3/o4/gpt-5 前缀模型。
/// 其余（DeepSeek chat、qwen、各种兼容网关）一律返回 None，调用方静默忽略。
pub fn thinking_kind(provider: &str, model_id: &str) -> Option<ThinkingKind> {
    let p = provider.trim().to_lowercase();
    if p == "anthropic" {
        return Some(ThinkingKind::AnthropicBudget);
    }
    let id = model_id.trim().to_lowercase();
    if p == "openai"
        && (id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") || id.starts_with("gpt-5"))
    {
        return Some(ThinkingKind::OpenAiEffort);
    }
    None
}

/// 档位 → Anthropic `thinking.budget_tokens`。低 / 中 / 高，其余（含空 = 默认）返回 None。
/// 保留既有 1024/2048/4096 档位；调用方必须保证 budget 严格小于实际输出预留。
pub fn thinking_budget_tokens(level: &str) -> Option<u32> {
    match level.trim().to_lowercase().as_str() {
        "low" => Some(1024),
        "medium" => Some(2048),
        "high" => Some(4096),
        _ => None,
    }
}

/// Anthropic 要求 `max_tokens > budget_tokens`：开思考时把输出上限抬到 budget + 余量。
/// `ceiling` 是 agent.rs 的输出天花板；抬不上去就返回 None（调用方不该开思考）。
pub fn thinking_max_tokens(max_tokens: i64, budget: u32, ceiling: i64) -> Option<i64> {
    let want = (budget as i64).saturating_add(1024);
    if want > ceiling {
        return None;
    }
    Some(max_tokens.max(want).min(ceiling))
}

/// 三态 `visionMode` 的最终判定：手动锁定优先，`auto` / 缺省才用规则库兜底。
///
/// - `text`：用户明确声明该模型/线路不看图，严格不发图（规则库不能翻案）；
/// - `vision`：用户明确锁定可看图，即便名字像纯文本也照发；
/// - `auto` / 缺省：`vision || is_vision_default(..)`，未知模型保守判否，
///   避免向纯文本线路发图拿 4xx。与 [`is_vision_default`] 口径一致。
pub fn effective_vision(
    vision: bool,
    vision_mode: Option<&str>,
    provider: &str,
    model_id: &str,
) -> bool {
    match vision_mode.unwrap_or("").trim().to_lowercase().as_str() {
        "text" => false,
        "vision" => true,
        _ => vision || is_vision_default(provider, model_id),
    }
}

/// Compatibility command for exact published output capabilities. Request
/// defaults are resolved separately and are never fabricated as capabilities.
pub fn infer_max_output_tokens(_provider: &str, model_id: &str) -> Option<u32> {
    catalog_capability(model_id).map(|entry| entry.max_output_tokens as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_multimodal_family_is_vision() {
        assert_eq!(infer_modality("openai", "gpt-4o"), Modality::Vision);
        assert_eq!(infer_modality("openai", "gpt-4o-mini"), Modality::Vision);
        assert_eq!(infer_modality("openai", "gpt-4-turbo"), Modality::Vision);
        assert_eq!(infer_modality("openai", "gpt-4.1"), Modality::Vision);
        assert_eq!(infer_modality("openai", "chatgpt-4o-latest"), Modality::Vision);
    }

    #[test]
    fn openai_text_only_members_are_text() {
        assert_eq!(infer_modality("openai", "text-embedding-3-large"), Modality::Text);
        assert_eq!(infer_modality("openai", "o1-mini"), Modality::Text);
        assert_eq!(infer_modality("openai", "o3-mini"), Modality::Text);
        assert_eq!(infer_modality("openai", "whisper-1"), Modality::Text);
    }

    #[test]
    fn deepseek_chat_is_text_not_vision() {
        // deepseek-chat 不看图：否决词必须赢过任何噪声。
        assert_eq!(infer_modality("deepseek", "deepseek-chat"), Modality::Text);
        assert_eq!(infer_modality("deepseek", "deepseek-reasoner"), Modality::Text);
        assert_eq!(infer_modality("deepseek", "deepseek-r1"), Modality::Text);
    }

    #[test]
    fn anthropic_claude3_and_fallback_is_vision() {
        assert_eq!(infer_modality("anthropic", "claude-3-5-sonnet-20241022"), Modality::Vision);
        assert_eq!(infer_modality("anthropic", "claude-sonnet-4-20250514"), Modality::Vision);
        // 没带版本号也按厂商兜底
        assert_eq!(infer_modality("anthropic", "claude-whatever"), Modality::Vision);
    }

    #[test]
    fn qwen_vl_family_vs_text_members() {
        assert_eq!(infer_modality("qwen", "qwen2.5-vl-7b-instruct"), Modality::Vision);
        assert_eq!(infer_modality("qwen", "qwen-vl-max"), Modality::Vision);
        assert_eq!(infer_modality("qwen", "qwen-turbo"), Modality::Text);
        assert_eq!(infer_modality("qwen", "qwen2.5-coder-32b"), Modality::Text);
    }

    #[test]
    fn open_vision_models_detected() {
        assert_eq!(infer_modality("siliconflow", "llava-1.5-7b"), Modality::Vision);
        assert_eq!(infer_modality("mistral", "pixtral-12b"), Modality::Vision);
        assert_eq!(infer_modality("zhipu", "glm-4v-plus"), Modality::Vision);
        assert_eq!(infer_modality("google", "gemini-1.5-pro"), Modality::Vision);
        assert_eq!(infer_modality("meta", "llama-3.2-90b-vision"), Modality::Vision);
    }

    #[test]
    fn unknown_when_no_signal() {
        assert_eq!(infer_modality("custom", "my-private-model-x"), Modality::Unknown);
        assert_eq!(infer_modality("", ""), Modality::Unknown);
    }

    #[test]
    fn is_vision_default_matches_inference() {
        assert!(is_vision_default("openai", "gpt-4o"));
        assert!(!is_vision_default("deepseek", "deepseek-chat"));
        assert!(!is_vision_default("custom", "mystery")); // Unknown → false（保守）
    }

    #[test]
    fn effective_vision_manual_lock_wins_over_rules() {
        // 手动「文本」压过看图名字：中转线路挂名 claude 也不能发图。
        assert!(!effective_vision(false, Some("text"), "anthropic", "claude-sonnet-4"));
        assert!(!effective_vision(true, Some("text"), "openai", "gpt-4o"));
        // 手动「看图」压过纯文本名字。
        assert!(effective_vision(false, Some("vision"), "deepseek", "deepseek-chat"));
        // 大小写 / 空白容错。
        assert!(!effective_vision(false, Some(" TEXT "), "anthropic", "claude-3"));
        assert!(effective_vision(false, Some("Vision"), "deepseek", "deepseek-chat"));
    }

    #[test]
    fn effective_vision_auto_falls_back_to_rules() {
        // auto + 存量 false + 名字能判多模态 → 自动放行（本次修复的核心场景）。
        assert!(effective_vision(false, Some("auto"), "anthropic", "claude-sonnet-4-20250514"));
        assert!(effective_vision(false, Some("auto"), "google", "gemini-2.0-flash"));
        // auto + 存量 true：用户勾过，直接采信。
        assert!(effective_vision(true, Some("auto"), "custom", "mystery"));
        // auto + 纯文本名字：规则库否决。
        assert!(!effective_vision(false, Some("auto"), "deepseek", "deepseek-chat"));
        // auto + 未知模型：保守判否。
        assert!(!effective_vision(false, Some("auto"), "custom", "mystery"));
        // 缺省 visionMode 等价 auto（旧快照 / 旧前端）。
        assert!(effective_vision(false, None, "anthropic", "claude-3-5-sonnet"));
        assert!(!effective_vision(false, None, "custom", "mystery"));
        assert!(effective_vision(true, None, "custom", "mystery"));
    }

    #[test]
    fn thinking_kind_only_for_protocols_that_take_it() {
        assert_eq!(thinking_kind("anthropic", "claude-sonnet-4"), Some(ThinkingKind::AnthropicBudget));
        assert_eq!(thinking_kind("Anthropic ", "claude-3-5"), Some(ThinkingKind::AnthropicBudget));
        assert_eq!(thinking_kind("openai", "o3"), Some(ThinkingKind::OpenAiEffort));
        assert_eq!(thinking_kind("openai", "gpt-5-mini"), Some(ThinkingKind::OpenAiEffort));
        // 不认这两条参数的线路：静默忽略
        assert_eq!(thinking_kind("openai", "gpt-4o"), None);
        assert_eq!(thinking_kind("deepseek", "deepseek-chat"), None);
        assert_eq!(thinking_kind("openrouter", "anthropic/claude-sonnet-4"), None);
        assert_eq!(thinking_kind("custom", "mystery"), None);
    }

    #[test]
    fn thinking_levels_map_to_budget_and_respect_max_tokens() {
        assert_eq!(thinking_budget_tokens("low"), Some(1024));
        assert_eq!(thinking_budget_tokens(" HIGH "), Some(4096));
        assert_eq!(thinking_budget_tokens(""), None);
        assert_eq!(thinking_budget_tokens("default"), None);
        // 抬到 budget + 1024，但不越过天花板
        assert_eq!(thinking_max_tokens(1536, 2048, 8192), Some(3072));
        assert_eq!(thinking_max_tokens(8192, 1024, 8192), Some(8192));
        // 想抬也抬不上去（budget 已接近天花板）→ 调用方不该开思考
        assert_eq!(thinking_max_tokens(4096, 8000, 8192), None);
    }

    #[test]
    fn modality_as_str_roundtrip() {
        assert_eq!(Modality::Vision.as_str(), "vision");
        assert_eq!(Modality::Text.as_str(), "text");
        assert_eq!(Modality::Unknown.as_str(), "unknown");
    }

    #[test]
    fn max_output_uses_exact_published_models_without_gateway_guesses() {
        assert_eq!(
            infer_max_output_tokens("mimo", "mimo-v2.6-flash"),
            None
        );
        assert_eq!(
            infer_max_output_tokens("openrouter", "openrouter/free"),
            None
        );
        assert_eq!(
            infer_max_output_tokens("deepseek", "deepseek-chat"),
            None
        );
        assert_eq!(
            infer_max_output_tokens("anthropic", "claude-sonnet-4-20250514"),
            Some(64_000)
        );
        assert_eq!(infer_max_output_tokens("custom", "mystery-x"), None);
        assert_eq!(infer_max_output_tokens("openai", ""), None);
        assert_eq!(infer_max_output_tokens("deepseek", "deepseek-v4-flash"), Some(384_000));
        assert_eq!(infer_max_output_tokens("custom", "sub2/deepseek-v4-flash"), None);
        assert_eq!(infer_max_output_tokens("custom", "deepseek-v4.1-flash"), None);
        assert_eq!(infer_max_output_tokens("openai", "gpt-4.1"), Some(32_768));
    }
}
