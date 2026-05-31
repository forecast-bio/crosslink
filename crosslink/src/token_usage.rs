//! Token usage parsing and cost estimation.
//!
//! Provides utilities for:
//! - Parsing token usage data from Claude API response metadata
//! - Estimating costs based on model pricing
//! - Extracting usage from kickoff report JSON

use serde::Deserialize;

/// Raw token usage data as reported by the Claude API.
#[derive(Debug, Clone, Deserialize)]
pub struct RawTokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<i64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<i64>,
}

/// A parsed token usage record ready for database insertion.
#[derive(Debug, Clone)]
pub struct ParsedUsage {
    pub agent_id: String,
    pub session_id: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub model: String,
    pub cost_estimate: Option<f64>,
}

/// Model pricing per million tokens (input, output).
/// Based on publicly available Anthropic pricing as of 2025.
struct ModelPricing {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_creation: f64,
}

fn get_pricing(model: &str) -> Option<ModelPricing> {
    // Normalize model name for matching
    let m = model.to_lowercase();

    // Claude models (Anthropic)
    if m.contains("opus") {
        Some(ModelPricing {
            input: 15.0,
            output: 75.0,
            cache_read: 1.5,
            cache_creation: 18.75,
        })
    } else if m.contains("sonnet") {
        Some(ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_creation: 3.75,
        })
    } else if m.contains("haiku") {
        Some(ModelPricing {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_creation: 1.0,
        })
    // Gemini models (Google) — prices per million tokens, verified 2026-05-31.
    // cache_read reflects context-cache hit rate; cache_creation is 0 (Google charges
    // storage by the hour rather than per-write, so write cost is not tracked here).
    // Patterns are checked most-specific first to avoid prefix collisions.
    } else if m.contains("gemini-3.5-flash") {
        Some(ModelPricing {
            input: 1.50,
            output: 9.00,
            cache_read: 0.15,
            cache_creation: 0.0,
        })
    // gemini-3.1-flash-lite must precede any broader "gemini-3.1-flash" check
    } else if m.contains("gemini-3.1-flash-lite") {
        Some(ModelPricing {
            input: 0.25,
            output: 1.50,
            cache_read: 0.025,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-3.1-pro") {
        Some(ModelPricing {
            input: 2.00,
            output: 12.00,
            cache_read: 0.20,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-2.5-pro") {
        Some(ModelPricing {
            input: 1.25,
            output: 10.00,
            cache_read: 0.125,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-2.5-flash") {
        Some(ModelPricing {
            input: 0.30,
            output: 2.50,
            cache_read: 0.03,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-2.0-flash") {
        Some(ModelPricing {
            input: 0.10,
            output: 0.40,
            cache_read: 0.025,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-1.5-pro") {
        Some(ModelPricing {
            input: 1.25,
            output: 5.00,
            cache_read: 0.3125,
            cache_creation: 0.0,
        })
    } else if m.contains("gemini-1.5-flash") {
        Some(ModelPricing {
            input: 0.075,
            output: 0.30,
            cache_read: 0.01875,
            cache_creation: 0.0,
        })
    } else {
        None
    }
}

/// Estimate cost in USD for a token usage record.
#[must_use]
pub fn estimate_cost(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
) -> Option<f64> {
    let pricing = get_pricing(model)?;
    #[allow(clippy::cast_precision_loss)] // token counts are well within f64 mantissa range
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input;
    #[allow(clippy::cast_precision_loss)]
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output;
    #[allow(clippy::cast_precision_loss)]
    let cache_read_cost =
        (cache_read_tokens.unwrap_or(0) as f64 / 1_000_000.0) * pricing.cache_read;
    #[allow(clippy::cast_precision_loss)]
    let cache_creation_cost =
        (cache_creation_tokens.unwrap_or(0) as f64 / 1_000_000.0) * pricing.cache_creation;
    Some(input_cost + output_cost + cache_read_cost + cache_creation_cost)
}

/// Parse a raw Claude API usage block into a `ParsedUsage`.
#[must_use]
pub fn parse_api_usage(
    raw: &RawTokenUsage,
    agent_id: &str,
    session_id: Option<i64>,
    model: &str,
) -> ParsedUsage {
    let cost = estimate_cost(
        model,
        raw.input_tokens,
        raw.output_tokens,
        raw.cache_read_input_tokens,
        raw.cache_creation_input_tokens,
    );
    ParsedUsage {
        agent_id: agent_id.to_string(),
        session_id,
        input_tokens: raw.input_tokens,
        output_tokens: raw.output_tokens,
        cache_read_tokens: raw.cache_read_input_tokens,
        cache_creation_tokens: raw.cache_creation_input_tokens,
        model: model.to_string(),
        cost_estimate: cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_cost_sonnet() {
        let cost = estimate_cost("claude-sonnet-4-20250514", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 3.0 + 15.0 = 18.0
        assert!((c - 18.0).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_opus() {
        let cost = estimate_cost("claude-opus-4-20250514", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 15.0 + 75.0 = 90.0
        assert!((c - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_haiku() {
        let cost = estimate_cost(
            "claude-haiku-4-5-20251001",
            1_000_000,
            1_000_000,
            None,
            None,
        );
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 0.80 + 4.0 = 4.80
        assert!((c - 4.80).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_with_cache() {
        let cost = estimate_cost(
            "claude-sonnet-4-20250514",
            500_000,
            200_000,
            Some(1_000_000),
            Some(300_000),
        );
        assert!(cost.is_some());
        let c = cost.unwrap();
        // input: 0.5 * 3.0 = 1.5
        // output: 0.2 * 15.0 = 3.0
        // cache_read: 1.0 * 0.3 = 0.3
        // cache_creation: 0.3 * 3.75 = 1.125
        let expected = 1.5 + 3.0 + 0.3 + 1.125;
        assert!((c - expected).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_unknown_model() {
        let cost = estimate_cost("gpt-4o", 1000, 500, None, None);
        assert!(cost.is_none());
    }

    #[test]
    fn test_estimate_cost_gemini_3_1_flash_lite() {
        let cost = estimate_cost("gemini-3.1-flash-lite", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 0.25 + 1.50 = 1.75
        assert!((c - 1.75).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_3_1_pro_preview() {
        let cost = estimate_cost("gemini-3.1-pro-preview", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 2.00 + 12.00 = 14.00
        assert!((c - 14.00).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_3_5_flash() {
        let cost = estimate_cost("gemini-3.5-flash", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 1.50 + 9.00 = 10.50
        assert!((c - 10.50).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_2_5_pro() {
        let cost = estimate_cost(
            "gemini-2.5-pro-preview-05-06",
            1_000_000,
            1_000_000,
            None,
            None,
        );
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 1.25 + 10.00 = 11.25
        assert!((c - 11.25).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_2_5_flash() {
        let cost = estimate_cost(
            "gemini-2.5-flash-preview-04-17",
            1_000_000,
            1_000_000,
            None,
            None,
        );
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 0.30 + 2.50 = 2.80
        assert!((c - 2.80).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_2_0_flash() {
        let cost = estimate_cost("gemini-2.0-flash", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 0.10 + 0.40 = 0.50
        assert!((c - 0.50).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_1_5_pro() {
        let cost = estimate_cost("gemini-1.5-pro-002", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 1.25 + 5.00 = 6.25
        assert!((c - 6.25).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_1_5_flash() {
        let cost = estimate_cost("gemini-1.5-flash-002", 1_000_000, 1_000_000, None, None);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // 0.075 + 0.30 = 0.375
        assert!((c - 0.375).abs() < 0.001);
    }

    #[test]
    fn test_estimate_cost_gemini_with_cache_read() {
        // Gemini 2.5 Pro with context cache hits; cache_creation should be 0
        let cost = estimate_cost(
            "gemini-2.5-pro",
            500_000,
            200_000,
            Some(1_000_000),
            Some(300_000),
        );
        assert!(cost.is_some());
        let c = cost.unwrap();
        // input: 0.5 * 1.25 = 0.625
        // output: 0.2 * 10.00 = 2.0
        // cache_read: 1.0 * 0.125 = 0.125
        // cache_creation: 0.3 * 0.0 = 0.0
        let expected = 0.625 + 2.0 + 0.125 + 0.0;
        assert!((c - expected).abs() < 0.001);
    }

    #[test]
    fn test_parse_api_usage() {
        let raw = RawTokenUsage {
            input_tokens: 5000,
            output_tokens: 1000,
            cache_read_input_tokens: Some(10000),
            cache_creation_input_tokens: None,
        };
        let parsed = parse_api_usage(&raw, "agent-1", Some(42), "claude-sonnet-4-20250514");
        assert_eq!(parsed.agent_id, "agent-1");
        assert_eq!(parsed.session_id, Some(42));
        assert_eq!(parsed.input_tokens, 5000);
        assert_eq!(parsed.output_tokens, 1000);
        assert_eq!(parsed.cache_read_tokens, Some(10000));
        assert!(parsed.cost_estimate.is_some());
        assert_eq!(parsed.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_raw_token_usage_deserialize() {
        let json = r#"{"input_tokens": 100, "output_tokens": 50}"#;
        let raw: RawTokenUsage = serde_json::from_str(json).unwrap();
        assert_eq!(raw.input_tokens, 100);
        assert_eq!(raw.output_tokens, 50);
        assert!(raw.cache_read_input_tokens.is_none());
    }

    #[test]
    fn test_raw_token_usage_deserialize_with_cache() {
        let json = r#"{
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 500
        }"#;
        let raw: RawTokenUsage = serde_json::from_str(json).unwrap();
        assert_eq!(raw.cache_read_input_tokens, Some(2000));
        assert_eq!(raw.cache_creation_input_tokens, Some(500));
    }
}
