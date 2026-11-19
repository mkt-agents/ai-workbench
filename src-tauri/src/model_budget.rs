//! Shared desktop request/compaction budget. No IO or provider requests.
use crate::ai_commands::AIModelConfig;
use crate::model_capability::{self, ThinkingKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBudget {
    pub model: String,
    pub model_name: String,
    pub model_window: usize,
    pub window_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_limit: Option<usize>,
    pub effective_window: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_max_output_tokens: Option<usize>,
    pub output_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_reference: Option<String>,
    pub output_mode: String,
    pub requested_output_tokens: usize,
    pub output_reserve: usize,
    pub schema_tokens: usize,
    pub input_budget: usize,
    pub compact_trigger: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<usize>,
    pub limitations: Vec<String>,
}

fn capability_cache(config: &AIModelConfig) -> Option<model_capability::ModelCapabilityCache> {
    config.capability_cache.as_ref()?.validated_for(
        &config.provider,
        &config.base_url,
        &config.model,
    )
}

fn window_details(config: &AIModelConfig) -> (usize, &'static str, Option<String>) {
    if let Some(window) = config
        .context_window_tokens
        .filter(|n| model_capability::valid_context_window(*n))
    {
        return (window, "manual", None);
    }
    if let Some(cache) = capability_cache(config) {
        if let Some(window) = cache.context_window_tokens {
            return (
                window,
                "provider",
                Some(format!("{}/models", cache.base_url)),
            );
        }
    }
    if let Some(entry) = model_capability::catalog_capability(&config.model) {
        return (
            entry.context_window_tokens,
            "catalog",
            Some(entry.reference.into()),
        );
    }
    (48_000, "fallback", None)
}

fn configured_limit(configured: usize) -> Option<usize> {
    (configured != 0).then(|| {
        configured.clamp(
            model_capability::MIN_CONTEXT_WINDOW,
            model_capability::MAX_CAPABILITY_TOKENS,
        )
    })
}

/// Independent of the output request: summaries can still recover a session
/// whose manual maxTokens leaves no room for a normal conversation request.
pub fn resolve_window(config: &AIModelConfig, configured: usize) -> usize {
    let model = window_details(config).0;
    configured_limit(configured).map_or(model, |limit| model.min(limit))
}

pub fn resolve_budget(
    config: &AIModelConfig,
    configured: usize,
    schema: usize,
    thinking: Option<&str>,
) -> Result<ModelBudget, String> {
    let (model_window, window_source, window_reference) = window_details(config);
    let configured_limit = configured_limit(configured);
    let effective_window = configured_limit.map_or(model_window, |limit| model_window.min(limit));
    let cache = capability_cache(config);
    let catalog = model_capability::catalog_capability(&config.model);
    let (model_max_output_tokens, output_source, output_reference) =
        if let Some(cache) = cache.as_ref().filter(|c| c.max_output_tokens.is_some()) {
            (
                cache.max_output_tokens,
                "provider",
                Some(format!("{}/models", cache.base_url)),
            )
        } else if let Some(entry) = catalog {
            (
                Some(entry.max_output_tokens),
                "catalog",
                Some(entry.reference.into()),
            )
        } else {
            (None, "unknown", None)
        };
    let auto = config
        .output_budget_mode
        .as_deref()
        .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("auto"));
    let requested_output_tokens = if auto {
        8192
    } else if config.max_tokens > 0 {
        usize::try_from(config.max_tokens).unwrap_or(usize::MAX)
    } else {
        4096
    };
    let mut limitations = Vec::new();
    let mut output_reserve = requested_output_tokens;
    if let Some(cap) = model_max_output_tokens {
        if output_reserve > cap {
            output_reserve = cap;
            limitations.push("model_output_limit".into());
        }
    }
    if auto && output_reserve > effective_window / 3 {
        output_reserve = effective_window / 3;
        limitations.push("small_window".into());
    }
    // Subtraction/checked comparison avoids wrapping on hostile imported values.
    let available = effective_window.checked_sub(schema)
        .filter(|available| output_reserve < *available)
        .ok_or_else(|| format!(
            "CONTEXT_OUTPUT_TOO_LARGE: output reserve ({output_reserve}) plus tool schemas ({schema}) must be smaller than the effective context window ({effective_window}); reduce output or increase the context limit"
        ))?;
    let mut thinking_budget = None;
    if model_capability::thinking_kind(&config.provider, &config.model)
        == Some(ThinkingKind::AnthropicBudget)
    {
        if let Some(tokens) = thinking.and_then(model_capability::thinking_budget_tokens) {
            let tokens = tokens as usize;
            let wanted = tokens + 1024;
            let ceiling = model_max_output_tokens.unwrap_or(usize::MAX).min(if auto {
                effective_window / 3
            } else {
                usize::MAX
            });
            if wanted <= ceiling && wanted < available {
                if output_reserve < wanted {
                    output_reserve = wanted;
                    limitations.push("thinking_adjusted".into());
                }
                thinking_budget = Some(tokens);
            } else {
                limitations.push("thinking_disabled".into());
            }
        }
    }
    let input_budget = available - output_reserve;
    Ok(ModelBudget {
        model: config.model.trim().into(),
        model_name: if config.name.trim().is_empty() {
            config.model.trim().into()
        } else {
            config.name.trim().into()
        },
        model_window,
        window_source: window_source.into(),
        window_reference,
        configured_limit,
        effective_window,
        model_max_output_tokens,
        output_source: output_source.into(),
        output_reference,
        output_mode: if auto { "auto" } else { "manual" }.into(),
        requested_output_tokens,
        output_reserve,
        schema_tokens: schema,
        input_budget,
        compact_trigger: input_budget * 4 / 5,
        thinking_budget,
        limitations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(extra: serde_json::Value) -> AIModelConfig {
        let mut value = json!({
            "name":"Test model", "provider":"custom", "baseUrl":"https://gateway.test/v1",
            "apiKey":"", "model":"private-model", "temperature":0.7,
            "maxTokens":4096, "isDefault":false
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn legacy_manual_and_auto_use_distinct_reserves() {
        let legacy = resolve_budget(&config(json!({})), 0, 1000, None).unwrap();
        assert_eq!(
            (
                legacy.model_window,
                legacy.output_reserve,
                legacy.input_budget
            ),
            (48_000, 4096, 42_904)
        );
        assert_eq!(legacy.output_mode, "manual");
        assert_eq!(legacy.window_source, "fallback");
        assert_eq!(legacy.output_source, "unknown");
        let auto = resolve_budget(
            &config(json!({"outputBudgetMode":"auto", "maxTokens":100000})),
            0,
            1000,
            None,
        )
        .unwrap();
        assert_eq!(
            (
                auto.requested_output_tokens,
                auto.output_reserve,
                auto.input_budget,
                auto.compact_trigger
            ),
            (8192, 8192, 38_808, 31_046)
        );
        let manual = resolve_budget(&config(json!({"maxTokens":24000})), 0, 0, None).unwrap();
        assert_eq!(manual.output_reserve, 24_000);
        assert_eq!(
            resolve_budget(&config(json!({"maxTokens":0})), 0, 0, None)
                .unwrap()
                .output_reserve,
            4096
        );
    }

    #[test]
    fn exact_catalogue_supplies_independent_window_and_output_sources() {
        let mut cfg = config(json!({"model":"deepseek-v4-flash", "outputBudgetMode":"auto"}));
        let budget = resolve_budget(&cfg, 0, 1000, None).unwrap();
        assert_eq!(
            (budget.model_window, budget.model_max_output_tokens),
            (1_000_000, Some(384_000))
        );
        assert_eq!(
            (&*budget.window_source, &*budget.output_source),
            ("catalog", "catalog")
        );
        assert!(budget
            .window_reference
            .unwrap()
            .contains("api-docs.deepseek.com"));
        cfg.context_window_tokens = Some(60_000);
        let budget = resolve_budget(&cfg, 48_000, 0, None).unwrap();
        assert_eq!(
            (
                budget.model_window,
                budget.effective_window,
                budget.configured_limit
            ),
            (60_000, 48_000, Some(48_000))
        );
        assert_eq!(
            (&*budget.window_source, &*budget.output_source),
            ("manual", "catalog")
        );
        for alias in ["sub2/deepseek-v4-flash", "deepseek-v4.1-flash", "my-gpt-4o"] {
            cfg.model = alias.into();
            cfg.context_window_tokens = None;
            let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
            assert_eq!(
                (budget.model_window, budget.model_max_output_tokens),
                (48_000, None)
            );
        }
    }

    #[test]
    fn provider_cache_is_identity_bound_and_resolves_each_field_separately() {
        let mut cfg = config(
            json!({"provider":" OpenAI ", "model":" gpt-4o ", "capabilityCache":{
            "provider":"openai", "baseUrl":" https://gateway.test/v1/// ", "model":"gpt-4o",
            "contextWindowTokens":90000, "maxOutputTokens":6000
        }, "outputBudgetMode":"auto"}),
        );
        let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
        assert_eq!((budget.model_window, budget.output_reserve), (90_000, 6000));
        assert_eq!(
            (&*budget.window_source, &*budget.output_source),
            ("provider", "provider")
        );
        assert_eq!(budget.limitations, ["model_output_limit"]);
        cfg.capability_cache.as_mut().unwrap().context_window_tokens = Some(0);
        let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
        assert_eq!(
            (budget.model_window, budget.model_max_output_tokens),
            (128_000, Some(6000))
        );
        assert_eq!(budget.window_source, "catalog");
        cfg.base_url = "https://other.test/v1".into();
        let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
        assert_eq!(
            (budget.model_window, budget.model_max_output_tokens),
            (128_000, Some(16_384))
        );
        cfg.base_url = "https://gateway.test/v1".into();
        cfg.provider = "anthropic".into();
        assert_eq!(
            resolve_budget(&cfg, 0, 0, None).unwrap().output_source,
            "catalog"
        );
        cfg.provider = "openai".into();
        cfg.model = "private-model".into();
        assert_eq!(
            resolve_budget(&cfg, 0, 0, None).unwrap().output_source,
            "unknown"
        );
    }

    #[test]
    fn output_limits_and_small_windows_report_the_effective_reason() {
        let cfg = config(json!({"model":"gpt-4o", "maxTokens":50000}));
        let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
        assert_eq!(
            (budget.requested_output_tokens, budget.output_reserve),
            (50_000, 16_384)
        );
        assert_eq!(budget.limitations, ["model_output_limit"]);
        let cfg = config(json!({"contextWindowTokens":4096, "outputBudgetMode":"auto"}));
        let budget = resolve_budget(&cfg, 0, 100, None).unwrap();
        assert_eq!(
            (
                budget.output_reserve,
                budget.input_budget,
                budget.compact_trigger
            ),
            (1365, 2631, 2104)
        );
        assert_eq!(budget.limitations, ["small_window"]);
    }

    #[test]
    fn exhausted_and_overflowing_budgets_fail_but_window_resolution_still_works() {
        let cfg = config(json!({"contextWindowTokens":4096, "maxTokens":4096}));
        assert!(resolve_budget(&cfg, 0, 0, None)
            .unwrap_err()
            .starts_with("CONTEXT_OUTPUT_TOO_LARGE"));
        assert!(resolve_budget(&cfg, 0, usize::MAX, None)
            .unwrap_err()
            .starts_with("CONTEXT_OUTPUT_TOO_LARGE"));
        assert_eq!(resolve_window(&cfg, 0), 4096);
        let budget = resolve_budget(
            &config(json!({"contextWindowTokens":4096, "maxTokens":4095})),
            0,
            0,
            None,
        )
        .unwrap();
        assert_eq!((budget.input_budget, budget.compact_trigger), (1, 0));
    }

    #[test]
    fn thinking_is_in_the_same_reserve_and_never_exceeds_model_or_window() {
        let cfg = config(json!({"provider":"anthropic", "maxTokens":4096}));
        let budget = resolve_budget(&cfg, 0, 0, Some("high")).unwrap();
        assert_eq!(
            (
                budget.output_reserve,
                budget.thinking_budget,
                budget.input_budget
            ),
            (5120, Some(4096), 42_880)
        );
        assert_eq!(budget.limitations, ["thinking_adjusted"]);
        let cfg = config(
            json!({"provider":"anthropic", "outputBudgetMode":"auto", "contextWindowTokens":4096}),
        );
        let budget = resolve_budget(&cfg, 0, 0, Some("high")).unwrap();
        assert_eq!(
            (budget.output_reserve, budget.thinking_budget),
            (1365, None)
        );
        assert_eq!(budget.limitations, ["small_window", "thinking_disabled"]);
        let cfg = config(
            json!({"provider":"anthropic", "outputBudgetMode":"auto", "capabilityCache":{
                "provider":"anthropic", "baseUrl":"https://gateway.test/v1", "model":"private-model", "maxOutputTokens":2048
            }}),
        );
        let budget = resolve_budget(&cfg, 0, 0, Some("high")).unwrap();
        assert_eq!(
            (budget.output_reserve, budget.thinking_budget),
            (2048, None)
        );
        assert_eq!(
            budget.limitations,
            ["model_output_limit", "thinking_disabled"]
        );
    }

    #[test]
    fn invalid_metadata_is_not_clamped_into_a_fictitious_capability() {
        let cfg = config(json!({"contextWindowTokens":2_000_001, "capabilityCache":{
            "provider":"custom", "baseUrl":"https://gateway.test/v1", "model":"private-model",
            "contextWindowTokens":4095, "maxOutputTokens":2_000_001
        }}));
        let budget = resolve_budget(&cfg, 0, 0, None).unwrap();
        assert_eq!(
            (budget.model_window, budget.model_max_output_tokens),
            (48_000, None)
        );
        assert_eq!(resolve_window(&cfg, 1), 4096);
        assert_eq!(resolve_window(&cfg, usize::MAX), 48_000);
        let cfg = config(json!({"contextWindowTokens":2_000_000, "maxTokens":1_999_999}));
        assert_eq!(resolve_budget(&cfg, 0, 0, None).unwrap().input_budget, 1);
    }
}
