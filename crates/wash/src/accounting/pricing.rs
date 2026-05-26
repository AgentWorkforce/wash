//! Pricing table for accounting cost estimation.
//!
//! Isolated behind [`estimate`] so the lookup logic can grow without touching
//! ingestion. Unknown models intentionally return `0.0` — accounting is best-effort
//! telemetry and must never block transcript ingestion.
//!
//! TODO: load prices from external configuration (e.g. `${RELAYBURN_HOME}/pricing.json`)
//! so model launches don't require a wash release.

use serde::{Deserialize, Serialize};

/// Token usage extracted from a Claude Code assistant turn.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    #[serde(rename = "inputTokens", default)]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens", default)]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens", default)]
    pub cache_read_tokens: u64,
}

/// Per-million-token prices for a single model, in USD.
#[derive(Debug, Clone, Copy)]
struct Price {
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_creation_per_mtok: f64,
    cache_read_per_mtok: f64,
}

/// Hardcoded table of public Claude 4.x list prices (USD per million tokens) as of
/// 2025/2026. Kept intentionally small — unknown models fall through to 0.0 so
/// ingestion never breaks on a new model id.
fn lookup(model: &str) -> Option<Price> {
    // Match by canonical family name; we accept either the full model id
    // (e.g. `claude-opus-4-5-20250101`) or the short family slug.
    let m = model.to_ascii_lowercase();
    if m.contains("opus-4") {
        return Some(Price {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_creation_per_mtok: 18.75,
            cache_read_per_mtok: 1.50,
        });
    }
    if m.contains("sonnet-4") {
        return Some(Price {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_creation_per_mtok: 3.75,
            cache_read_per_mtok: 0.30,
        });
    }
    if m.contains("haiku-4") {
        return Some(Price {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_creation_per_mtok: 1.25,
            cache_read_per_mtok: 0.10,
        });
    }
    None
}

/// Estimate the USD cost of an assistant turn. Returns `0.0` for unknown models
/// so the caller can still record the turn.
pub fn estimate(model: &str, usage: &Usage) -> f64 {
    let Some(p) = lookup(model) else {
        return 0.0;
    };
    let cost = (usage.input_tokens as f64) * p.input_per_mtok
        + (usage.output_tokens as f64) * p.output_per_mtok
        + (usage.cache_creation_tokens as f64) * p.cache_creation_per_mtok
        + (usage.cache_read_tokens as f64) * p.cache_read_per_mtok;
    cost / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_returns_zero() {
        let usage = Usage { input_tokens: 1000, output_tokens: 500, ..Default::default() };
        assert_eq!(estimate("claude-future-9000", &usage), 0.0);
        assert_eq!(estimate("", &usage), 0.0);
    }

    #[test]
    fn opus_4_pricing_uses_table() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        };
        // 1M input tokens @ $15 -> $15.00 exactly
        let cost = estimate("claude-opus-4-7", &usage);
        assert!((cost - 15.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn sonnet_4_combined_components() {
        let usage = Usage {
            input_tokens: 1_000,
            output_tokens: 2_000,
            cache_creation_tokens: 4_000,
            cache_read_tokens: 8_000,
        };
        // 1k * 3 + 2k * 15 + 4k * 3.75 + 8k * 0.30 = 3000 + 30000 + 15000 + 2400
        // = 50_400 / 1M = 0.0504
        let cost = estimate("claude-sonnet-4-5", &usage);
        assert!((cost - 0.0504).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn haiku_4_lookup() {
        let usage = Usage { input_tokens: 1_000_000, ..Default::default() };
        let cost = estimate("claude-haiku-4-1-20260101", &usage);
        assert!((cost - 1.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn zero_usage_costs_nothing() {
        assert_eq!(estimate("claude-opus-4-7", &Usage::default()), 0.0);
    }
}
