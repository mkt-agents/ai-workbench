//! 模型模态（文本 / 多模态）的离线启发式识别。
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

/// ACP / 部分 OpenAI 兼容网关拒绝的 completion 上限（与 wecom_dsh_profile 一致）。
pub const MAX_SAFE_COMPLETION_TOKENS: u32 = 131_072;

/// 离线推断「单次回答最长输出」token 上限，供模型广场表单预填。
/// 返回 None 时前端保持默认 4096。**不要**把上下文窗口当输出上限。
pub fn infer_max_output_tokens(provider: &str, model_id: &str) -> Option<u32> {
    let id = model_id.trim().to_lowercase();
    if id.is_empty() {
        return None;
    }
    let p = provider.trim().to_lowercase();

    // 免费 / 路由类：保守
    if id.contains("/free") || id.ends_with(":free") || id == "openrouter/free" || id == "auto" {
        return Some(8_192);
    }

    // 按模型名片段（优先于厂商默认）
    if id.contains("mimo") {
        return Some(MAX_SAFE_COMPLETION_TOKENS);
    }
    if id.contains("agnes") {
        return Some(16_384);
    }
    if id.contains("deepseek") {
        return Some(8_192);
    }
    if id.contains("claude") {
        return Some(64_000);
    }
    if id.contains("gpt-5") || id.contains("o1") || id.contains("o3") || id.contains("o4") {
        return Some(32_768);
    }
    if id.contains("gpt-4o") || id.contains("gpt-4.1") || id.contains("chatgpt-4o") {
        return Some(16_384);
    }
    if id.contains("gemini") {
        return Some(8_192);
    }
    if id.contains("grok") {
        return Some(32_768);
    }
    if id.contains("qwen") || id.contains("glm-") || id.contains("moonshot") || id.contains("kimi") {
        return Some(8_192);
    }
    if id.contains("llama") || id.contains("mistral") || id.contains("mixtral") {
        return Some(8_192);
    }

    // 厂商兜底
    match p.as_str() {
        "mimo" => Some(MAX_SAFE_COMPLETION_TOKENS),
        "agnes" => Some(16_384),
        "deepseek" => Some(8_192),
        "anthropic" => Some(64_000),
        "openai" => Some(16_384),
        "google" => Some(8_192),
        "xai" => Some(32_768),
        "openrouter" => Some(8_192),
        "moonshot" | "qwen" | "zhipu" | "groq" | "mistral" | "siliconflow" | "together"
        | "longcat" | "ollama" => Some(8_192),
        _ => None,
    }
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
    fn modality_as_str_roundtrip() {
        assert_eq!(Modality::Vision.as_str(), "vision");
        assert_eq!(Modality::Text.as_str(), "text");
        assert_eq!(Modality::Unknown.as_str(), "unknown");
    }

    #[test]
    fn max_output_mimo_and_free_routes() {
        assert_eq!(
            infer_max_output_tokens("mimo", "mimo-v2.6-flash"),
            Some(MAX_SAFE_COMPLETION_TOKENS)
        );
        assert_eq!(
            infer_max_output_tokens("openrouter", "openrouter/free"),
            Some(8_192)
        );
        assert_eq!(
            infer_max_output_tokens("deepseek", "deepseek-chat"),
            Some(8_192)
        );
        assert_eq!(
            infer_max_output_tokens("anthropic", "claude-sonnet-4-20250514"),
            Some(64_000)
        );
        assert_eq!(infer_max_output_tokens("custom", "mystery-x"), None);
        assert_eq!(infer_max_output_tokens("openai", ""), None);
    }
}
