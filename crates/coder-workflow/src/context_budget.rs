use coder_config::ResolvedAgentRuntimePolicy;

pub const TOOL_RESULT_GROWTH_ESTIMATE_TOKENS: u32 = 15_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub configured_context_window_tokens: u32,
    pub configured_auto_compact_token_limit: Option<u32>,
    pub autocompact_threshold_tokens: u32,
    pub blocking_limit_tokens: u32,
    pub estimated_max_turn_growth_tokens: u32,
}

pub fn context_budget_for_runtime(runtime: &ResolvedAgentRuntimePolicy) -> ContextBudget {
    let context_window_tokens = runtime.context_window_tokens;
    let codex_default_limit = context_window_tokens.saturating_mul(9) / 10;
    let autocompact_threshold_tokens = runtime
        .auto_compact_token_limit
        .min(codex_default_limit)
        .min(runtime.effective_context_window_tokens);

    ContextBudget {
        configured_context_window_tokens: context_window_tokens,
        configured_auto_compact_token_limit: Some(runtime.auto_compact_token_limit),
        autocompact_threshold_tokens,
        blocking_limit_tokens: runtime.effective_context_window_tokens,
        estimated_max_turn_growth_tokens: runtime
            .max_output_tokens
            .min(runtime.compact_output_reserve_tokens)
            .saturating_add(TOOL_RESULT_GROWTH_ESTIMATE_TOKENS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coder_config::{
        resolve_agent_runtime_policy, AgentRuntimePolicy, ModelCapabilities, ModelSpec,
    };

    fn resolved_runtime(runtime: AgentRuntimePolicy) -> ResolvedAgentRuntimePolicy {
        resolve_agent_runtime_policy(
            &ModelSpec {
                provider: "openai-compatible".to_owned(),
                model: "test".to_owned(),
                base_url_env: None,
                api_key_env: None,
                capabilities: ModelCapabilities::default(),
            },
            &runtime,
        )
    }

    #[test]
    fn context_budget_uses_codex_ninety_percent_default() {
        let runtime = resolved_runtime(AgentRuntimePolicy {
            max_output_tokens: Some(8_000),
            ..AgentRuntimePolicy::default()
        });

        let budget = context_budget_for_runtime(&runtime);

        assert_eq!(budget.autocompact_threshold_tokens, 180_000);
        assert_eq!(budget.blocking_limit_tokens, 190_000);
        assert_eq!(budget.estimated_max_turn_growth_tokens, 23_000);
    }

    #[test]
    fn context_budget_clamps_configured_limit_to_codex_ceiling() {
        let runtime = resolved_runtime(AgentRuntimePolicy {
            max_output_tokens: Some(8_000),
            ..AgentRuntimePolicy::default()
        });
        let budget = context_budget_for_runtime(&runtime);

        assert_eq!(budget.configured_auto_compact_token_limit, Some(180_000));
        assert_eq!(budget.autocompact_threshold_tokens, 180_000);
    }
}
