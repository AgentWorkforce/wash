//! Single shared token estimator used by every metrics surface (post_tool_observe
//! `tool_metrics` events, future aggregator passes, etc.). The estimator is
//! intentionally simple: ~4 bytes per token, which tracks closely enough to BPE
//! tokenizers for English text and source code that ratios across calls stay
//! meaningful. All callers must funnel through this helper so saved-token deltas
//! are comparable across tools.

pub const BYTES_PER_TOKEN: u64 = 4;

pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

pub fn estimate_tokens_usize(bytes: usize) -> u64 {
    estimate_tokens(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_bytes_zero_tokens() {
        assert_eq!(estimate_tokens(0), 0);
    }

    #[test]
    fn rounds_up_partial_token() {
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(3), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
    }

    #[test]
    fn large_input_matches_ratio() {
        assert_eq!(estimate_tokens(4_000), 1_000);
        assert_eq!(estimate_tokens(4_001), 1_001);
    }
}
