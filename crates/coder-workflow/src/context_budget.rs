use std::env;

use coder_config::{AgentRuntimePolicy, AGENT_AUTOCOMPACT_BUFFER_TOKENS_DEFAULT};

pub const WARNING_THRESHOLD_BUFFER_TOKENS: u32 = 20_000;
pub const ERROR_THRESHOLD_BUFFER_TOKENS: u32 = 20_000;
pub const MANUAL_COMPACT_BUFFER_TOKENS: u32 = 3_000;
pub const TOOL_RESULT_GROWTH_ESTIMATE_TOKENS: u32 = 15_000;
pub const LARGE_CONTEXT_WINDOW_TOKENS: u32 = 400_000;
pub const EXTRA_LARGE_CONTEXT_WINDOW_TOKENS: u32 = 800_000;
pub const LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS: u32 = 30_000;
pub const EXTRA_LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS: u32 = 50_000;
pub const CODER_AUTO_COMPACT_WINDOW_ENV: &str = "CODER_AUTO_COMPACT_WINDOW";
pub const CLAUDE_AUTO_COMPACT_WINDOW_ENV: &str = "CLAUDE_CODE_AUTO_COMPACT_WINDOW";
pub const CODER_AUTOCOMPACT_PCT_OVERRIDE_ENV: &str = "CODER_AUTOCOMPACT_PCT_OVERRIDE";
pub const CLAUDE_AUTOCOMPACT_PCT_OVERRIDE_ENV: &str = "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE";
pub const CODER_BLOCKING_LIMIT_OVERRIDE_ENV: &str = "CODER_BLOCKING_LIMIT_OVERRIDE";
pub const CLAUDE_BLOCKING_LIMIT_OVERRIDE_ENV: &str = "CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub configured_context_window_tokens: u32,
    pub configured_autocompact_buffer_tokens: u32,
    pub context_window_override_tokens: Option<u32>,
    pub effective_context_window_tokens: u32,
    pub effective_autocompact_buffer_tokens: u32,
    pub autocompact_threshold_tokens: u32,
    pub autocompact_threshold_overridden: bool,
    pub warning_threshold_tokens: u32,
    pub error_threshold_tokens: u32,
    pub blocking_limit_tokens: u32,
    pub blocking_limit_overridden: bool,
    pub estimated_max_turn_growth_tokens: u32,
}

pub fn context_budget_for_runtime(runtime: &AgentRuntimePolicy) -> ContextBudget {
    context_budget_for_runtime_with_overrides(runtime, ContextBudgetOverrides::from_env())
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ContextBudgetOverrides {
    auto_compact_window_tokens: Option<u32>,
    autocompact_pct_override: Option<f64>,
    blocking_limit_override_tokens: Option<u32>,
}

fn context_budget_for_runtime_with_overrides(
    runtime: &AgentRuntimePolicy,
    overrides: ContextBudgetOverrides,
) -> ContextBudget {
    let configured_context_window_tokens = runtime.context_window_tokens;
    let context_window_tokens = overrides
        .auto_compact_window_tokens
        .map(|override_tokens| configured_context_window_tokens.min(override_tokens))
        .unwrap_or(configured_context_window_tokens);
    let summary_reserve = runtime
        .compact_output_reserve_tokens
        .min(context_window_tokens);
    let effective_context_window_tokens = context_window_tokens.saturating_sub(summary_reserve);
    let effective_autocompact_buffer_tokens = effective_autocompact_buffer_tokens(
        runtime.autocompact_buffer_tokens,
        context_window_tokens,
    );
    let default_autocompact_threshold =
        effective_context_window_tokens.saturating_sub(effective_autocompact_buffer_tokens);
    let (autocompact_threshold_tokens, autocompact_threshold_overridden) =
        if let Some(percent) = overrides.autocompact_pct_override {
            let percentage_threshold =
                ((effective_context_window_tokens as f64) * (percent / 100.0)).floor() as u32;
            (
                percentage_threshold.min(default_autocompact_threshold),
                true,
            )
        } else {
            (default_autocompact_threshold, false)
        };
    let default_blocking_limit =
        effective_context_window_tokens.saturating_sub(MANUAL_COMPACT_BUFFER_TOKENS);
    let (blocking_limit_tokens, blocking_limit_overridden) =
        if let Some(override_tokens) = overrides.blocking_limit_override_tokens {
            (override_tokens, true)
        } else {
            (default_blocking_limit, false)
        };

    ContextBudget {
        configured_context_window_tokens,
        configured_autocompact_buffer_tokens: runtime.autocompact_buffer_tokens,
        context_window_override_tokens: overrides.auto_compact_window_tokens,
        effective_context_window_tokens,
        effective_autocompact_buffer_tokens,
        autocompact_threshold_tokens,
        autocompact_threshold_overridden,
        warning_threshold_tokens: effective_context_window_tokens
            .saturating_sub(WARNING_THRESHOLD_BUFFER_TOKENS),
        error_threshold_tokens: effective_context_window_tokens
            .saturating_sub(ERROR_THRESHOLD_BUFFER_TOKENS),
        blocking_limit_tokens,
        blocking_limit_overridden,
        estimated_max_turn_growth_tokens: runtime
            .max_output_tokens
            .unwrap_or(runtime.compact_output_reserve_tokens)
            .min(runtime.compact_output_reserve_tokens)
            .saturating_add(TOOL_RESULT_GROWTH_ESTIMATE_TOKENS),
    }
}

fn effective_autocompact_buffer_tokens(
    configured_buffer_tokens: u32,
    context_window_tokens: u32,
) -> u32 {
    if configured_buffer_tokens != AGENT_AUTOCOMPACT_BUFFER_TOKENS_DEFAULT {
        return configured_buffer_tokens;
    }
    if context_window_tokens >= EXTRA_LARGE_CONTEXT_WINDOW_TOKENS {
        return EXTRA_LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS;
    }
    if context_window_tokens >= LARGE_CONTEXT_WINDOW_TOKENS {
        return LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS;
    }
    configured_buffer_tokens
}

impl ContextBudgetOverrides {
    fn from_env() -> Self {
        Self {
            auto_compact_window_tokens: parse_positive_u32_env(
                CODER_AUTO_COMPACT_WINDOW_ENV,
                CLAUDE_AUTO_COMPACT_WINDOW_ENV,
            ),
            autocompact_pct_override: parse_percent_env(
                CODER_AUTOCOMPACT_PCT_OVERRIDE_ENV,
                CLAUDE_AUTOCOMPACT_PCT_OVERRIDE_ENV,
            ),
            blocking_limit_override_tokens: parse_positive_u32_env(
                CODER_BLOCKING_LIMIT_OVERRIDE_ENV,
                CLAUDE_BLOCKING_LIMIT_OVERRIDE_ENV,
            ),
        }
    }
}

fn parse_positive_u32_env(primary: &str, alias: &str) -> Option<u32> {
    env::var(primary)
        .ok()
        .or_else(|| env::var(alias).ok())
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
}

fn parse_percent_env(primary: &str, alias: &str) -> Option<f64> {
    env::var(primary)
        .ok()
        .or_else(|| env::var(alias).ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| *value > 0.0 && *value <= 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_budget_uses_claude_buffers_and_runtime_reserve() {
        let runtime = AgentRuntimePolicy {
            max_output_tokens: Some(8_000),
            ..AgentRuntimePolicy::default()
        };

        let budget = context_budget_for_runtime(&runtime);

        assert_eq!(budget.effective_context_window_tokens, 180_000);
        assert_eq!(budget.effective_autocompact_buffer_tokens, 13_000);
        assert_eq!(budget.autocompact_threshold_tokens, 167_000);
        assert!(!budget.autocompact_threshold_overridden);
        assert_eq!(budget.warning_threshold_tokens, 160_000);
        assert_eq!(budget.error_threshold_tokens, 160_000);
        assert_eq!(budget.blocking_limit_tokens, 177_000);
        assert!(!budget.blocking_limit_overridden);
        assert_eq!(budget.estimated_max_turn_growth_tokens, 23_000);
    }

    #[test]
    fn context_budget_uses_claude_dynamic_buffers_for_large_windows() {
        let large = AgentRuntimePolicy {
            context_window_tokens: 450_000,
            ..AgentRuntimePolicy::default()
        };
        let extra_large = AgentRuntimePolicy {
            context_window_tokens: 900_000,
            ..AgentRuntimePolicy::default()
        };

        let large_budget =
            context_budget_for_runtime_with_overrides(&large, ContextBudgetOverrides::default());
        let extra_large_budget = context_budget_for_runtime_with_overrides(
            &extra_large,
            ContextBudgetOverrides::default(),
        );

        assert_eq!(
            large_budget.effective_autocompact_buffer_tokens,
            LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS
        );
        assert_eq!(large_budget.autocompact_threshold_tokens, 400_000);
        assert_eq!(
            extra_large_budget.effective_autocompact_buffer_tokens,
            EXTRA_LARGE_CONTEXT_AUTOCOMPACT_BUFFER_TOKENS
        );
        assert_eq!(extra_large_budget.autocompact_threshold_tokens, 830_000);
    }

    #[test]
    fn context_budget_honors_claude_style_overrides() {
        let runtime = AgentRuntimePolicy {
            context_window_tokens: 200_000,
            max_output_tokens: Some(8_000),
            ..AgentRuntimePolicy::default()
        };
        let budget = context_budget_for_runtime_with_overrides(
            &runtime,
            ContextBudgetOverrides {
                auto_compact_window_tokens: Some(120_000),
                autocompact_pct_override: Some(50.0),
                blocking_limit_override_tokens: Some(99_000),
            },
        );

        assert_eq!(budget.context_window_override_tokens, Some(120_000));
        assert_eq!(budget.effective_context_window_tokens, 100_000);
        assert_eq!(budget.autocompact_threshold_tokens, 50_000);
        assert!(budget.autocompact_threshold_overridden);
        assert_eq!(budget.blocking_limit_tokens, 99_000);
        assert!(budget.blocking_limit_overridden);
    }
}
