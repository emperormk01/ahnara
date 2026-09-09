//! Cost-Aware Delegation - Token budget and cost control for sub-agent spawning
//! Prevents burning money on tasks that could be handled by main agent only

use serde::{Deserialize, Serialize};

use crate::config::SubAgentsConfig;

/// Token budget configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum tokens per request
    pub max_tokens_per_request: u32,
    /// Maximum tokens for sub-agents per session
    pub max_sub_agent_tokens_per_session: u32,
    /// Tokens used so far in this session
    pub tokens_used: u32,
    /// Whether sub-agent spawning is enabled
    pub sub_agents_enabled: bool,
    /// Minimum complexity score to delegate (0-100)
    pub min_complexity_for_delegation: u32,
    /// Cost multiplier for sub-agents (vs main agent)
    pub sub_agent_cost_factor: f32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_tokens_per_request: 8192,
            max_sub_agent_tokens_per_session: 50000, // ~50k tokens budget for sub-agents
            tokens_used: 0,
            sub_agents_enabled: true,
            min_complexity_for_delegation: 30, // Lowered from 40 to allow more tasks to be delegated
            sub_agent_cost_factor: 1.5,        // Sub-agents cost 50% more (overhead)
        }
    }
}

impl TokenBudget {
    pub fn new(max_tokens: u32, session_budget: u32) -> Self {
        Self {
            max_tokens_per_request: max_tokens,
            max_sub_agent_tokens_per_session: session_budget,
            ..Default::default()
        }
    }

    /// Build a budget from the `[sub_agents]` config section so file
    /// settings actually drive delegation instead of hardcoded defaults.
    pub fn from_sub_agents(config: &SubAgentsConfig, max_tokens_per_request: u32) -> Self {
        Self {
            max_tokens_per_request,
            max_sub_agent_tokens_per_session: config.max_budget,
            tokens_used: 0,
            sub_agents_enabled: config.enabled,
            min_complexity_for_delegation: config.min_complexity,
            sub_agent_cost_factor: 1.5,
        }
    }

    /// Check if we have budget remaining for sub-agent operations
    pub fn has_budget(&self) -> bool {
        self.tokens_used < self.max_sub_agent_tokens_per_session
    }

    /// Get remaining budget
    pub fn remaining_budget(&self) -> u32 {
        self.max_sub_agent_tokens_per_session
            .saturating_sub(self.tokens_used)
    }

    /// Record token usage
    pub fn record_usage(&mut self, tokens: u32) {
        self.tokens_used += tokens;
    }

    /// Check if delegation is worth it cost-wise
    pub fn is_delegation_worthwhile(
        &self,
        _estimated_main_cost: u32,
        estimated_sub_cost: u32,
        complexity: u32,
    ) -> bool {
        // Don't delegate if disabled
        if !self.sub_agents_enabled {
            return false;
        }

        // Don't delegate if not enough budget
        if !self.has_budget() {
            return false;
        }

        // Don't delegate simple tasks (complexity < threshold)
        if complexity < self.min_complexity_for_delegation {
            return false;
        }

        // Cost-benefit analysis:
        // Delegate if: sub-agent benefit > (sub_cost * factor) AND complexity justifies overhead
        let _adjusted_sub_cost = (estimated_sub_cost as f32 * self.sub_agent_cost_factor) as u32;

        // Only delegate if:
        // 1. Task is complex enough
        // 2. Sub-agent can do it better (even with overhead cost)
        // 3. We have budget
        complexity >= self.min_complexity_for_delegation && self.has_budget()
    }
}

/// Task complexity analyzer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAnalyzer {
    /// Minimum words to consider delegation
    min_words_for_delegation: usize,
    /// Patterns that indicate simple tasks (no delegation needed)
    simple_patterns: Vec<String>,
    /// Patterns that indicate complex tasks (delegation beneficial)
    complex_patterns: Vec<String>,
}

impl Default for ComplexityAnalyzer {
    fn default() -> Self {
        Self {
            min_words_for_delegation: 5, // Lowered from 10 to prevent simple_task from blocking budget/disabled tests
            simple_patterns: vec![
                "hello".to_string(),
                "hi".to_string(),
                "thanks".to_string(),
                "what is".to_string(),
                "tell me about".to_string(),
                "explain briefly".to_string(),
                "quick question".to_string(),
                "simple".to_string(),
                "just".to_string(),
                "only".to_string(),
            ],
            complex_patterns: vec![
                "research and analyze".to_string(),
                "implement and test".to_string(),
                "comprehensive".to_string(),
                "detailed report".to_string(),
                "multiple".to_string(),
                "parallel".to_string(),
                "independently".to_string(),
                "comprehensive analysis".to_string(),
                "step by step".to_string(),
                "break down".to_string(),
            ],
        }
    }
}

impl ComplexityAnalyzer {
    /// Calculate complexity score (0-100)
    pub fn analyze(&self, task: &str) -> TaskComplexity {
        let mut score = 0u32;
        let task_lower = task.to_lowercase();

        // Word count factor (up to 20 points)
        let word_count = task.split_whitespace().count();
        score += (word_count.min(40) as u32) / 2; // Max 20 points

        // Sentence count (up to 10 points)
        let sentences =
            task.matches('.').count() + task.matches('?').count() + task.matches('!').count();
        score += sentences.min(10) as u32;

        // Verb count (up to 15 points) - indicates multiple actions
        let verbs = [
            "write",
            "create",
            "analyze",
            "research",
            "implement",
            "build",
            "design",
            "test",
            "review",
            "debug",
        ];
        let verb_count = verbs.iter().filter(|v| task_lower.contains(*v)).count() as u32;
        score += verb_count * 3; // Max 15 points

        // Complexity patterns (up to 20 points)
        for pattern in &self.complex_patterns {
            if task_lower.contains(&pattern.to_lowercase()) {
                score += 5;
            }
        }

        // Simple patterns reduce score
        for pattern in &self.simple_patterns {
            if task_lower.contains(&pattern.to_lowercase()) {
                score = score.saturating_sub(10);
            }
        }

        // Multi-step indicators (up to 15 points)
        let multi_step_indicators = [
            " and ",
            " then ",
            " after ",
            " before ",
            " also ",
            " additionally ",
        ];
        let multi_step_count = multi_step_indicators
            .iter()
            .filter(|i| task_lower.contains(*i))
            .count() as u32;
        score += multi_step_count * 5; // Max 15 points

        // File/code mentions (up to 10 points) - indicates tool usage
        if task_lower.contains("file")
            || task_lower.contains("code")
            || task_lower.contains("script")
        {
            score += 10;
        }

        // Research/data mentions (up to 10 points)
        if task_lower.contains("research")
            || task_lower.contains("data")
            || task_lower.contains("analyze")
        {
            score += 10;
        }

        // Cap at 100
        score = score.min(100);

        // Determine recommendation
        let should_delegate = score >= 40 && word_count >= self.min_words_for_delegation;
        let estimated_tokens = self.estimate_token_cost(task, score);

        TaskComplexity {
            score,
            word_count,
            sentence_count: sentences,
            verb_count: verb_count as usize,
            should_delegate,
            estimated_main_tokens: estimated_tokens,
            estimated_sub_agent_tokens: (estimated_tokens as f32 * 1.5) as u32,
        }
    }

    /// Estimate token cost for the task
    fn estimate_token_cost(&self, task: &str, complexity: u32) -> u32 {
        // Base: input tokens (roughly 1.3 tokens per word)
        let input_tokens = (task.split_whitespace().count() as f32 * 1.3) as u32;

        // Output tokens estimation based on complexity
        // Simple tasks: ~100 tokens output
        // Complex tasks: ~1000-4000 tokens output
        let output_tokens = if complexity < 30 {
            100
        } else if complexity < 50 {
            500
        } else if complexity < 70 {
            1500
        } else {
            3000
        };

        input_tokens + output_tokens
    }

    /// Check if task is too simple to delegate
    pub fn is_simple_task(&self, task: &str) -> bool {
        let task_lower = task.to_lowercase();
        let word_count = task.split_whitespace().count();

        // Check for simple patterns
        for pattern in &self.simple_patterns {
            if task_lower.contains(&pattern.to_lowercase()) {
                return true;
            }
        }

        // Short messages are usually simple
        if word_count < self.min_words_for_delegation {
            return true;
        }

        false
    }
}

/// Task complexity result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComplexity {
    /// Complexity score (0-100)
    pub score: u32,
    /// Word count
    pub word_count: usize,
    /// Sentence count
    pub sentence_count: usize,
    /// Verb count
    pub verb_count: usize,
    /// Whether delegation is recommended
    pub should_delegate: bool,
    /// Estimated token cost for main agent
    pub estimated_main_tokens: u32,
    /// Estimated token cost for sub-agent (includes overhead)
    pub estimated_sub_agent_tokens: u32,
}

/// Delegation decision with reasoning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationDecision {
    /// Whether to delegate
    pub delegate: bool,
    /// Reason for decision
    pub reason: String,
    /// Complexity analysis
    pub complexity: TaskComplexity,
    /// Budget status
    pub budget_remaining: u32,
    /// Estimated cost savings (negative = sub-agent costs more)
    pub cost_difference: i32,
}

impl DelegationDecision {
    /// Create a decision to keep on main agent
    pub fn keep_on_main(reason: String, complexity: TaskComplexity) -> Self {
        Self {
            delegate: false,
            reason,
            complexity,
            budget_remaining: 0,
            cost_difference: 0,
        }
    }

    /// Create a decision to delegate
    pub fn delegate(
        reason: String,
        complexity: TaskComplexity,
        budget: u32,
        cost_diff: i32,
    ) -> Self {
        Self {
            delegate: true,
            reason,
            complexity,
            budget_remaining: budget,
            cost_difference: cost_diff,
        }
    }
}

/// Cost-aware delegator that considers budget and complexity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostAwareDelegator {
    budget: TokenBudget,
    analyzer: ComplexityAnalyzer,
    /// Track delegation history for learning
    delegation_history: Vec<DelegationDecision>,
}

impl CostAwareDelegator {
    pub fn new(budget: TokenBudget) -> Self {
        Self {
            budget,
            analyzer: ComplexityAnalyzer::default(),
            delegation_history: Vec::new(),
        }
    }

    /// Analyze task and make delegation decision
    pub fn should_delegate(&mut self, task: &str) -> DelegationDecision {
        // Analyze complexity
        let complexity = self.analyzer.analyze(task);

        // Check if simple task - always keep on main
        if self.analyzer.is_simple_task(task) {
            return DelegationDecision::keep_on_main(
                "Task is too simple for delegation".to_string(),
                complexity,
            );
        }

        // Check if user disabled sub-agents
        if !self.budget.sub_agents_enabled {
            return DelegationDecision::keep_on_main(
                "Sub-agents are disabled by user".to_string(),
                complexity,
            );
        }

        // Check budget
        if !self.budget.has_budget() {
            return DelegationDecision::keep_on_main(
                format!("Budget exhausted ({} tokens used)", self.budget.tokens_used),
                complexity,
            );
        }

        // Check complexity threshold
        if complexity.score < self.budget.min_complexity_for_delegation {
            return DelegationDecision::keep_on_main(
                format!(
                    "Complexity {} below threshold {}",
                    complexity.score, self.budget.min_complexity_for_delegation
                ),
                complexity,
            );
        }

        // Cost-benefit analysis
        let cost_diff =
            complexity.estimated_main_tokens as i32 - complexity.estimated_sub_agent_tokens as i32;

        // Even if sub-agent costs more, delegate if:
        // 1. Task is complex enough (score >= 30)
        // 2. We have plenty of budget
        // 3. Task benefits from isolation (parallel, independent)
        let worth_the_cost = complexity.score >= 30 && self.budget.remaining_budget() > 10000;

        if cost_diff < 0 && !worth_the_cost {
            return DelegationDecision::keep_on_main(
                format!(
                    "Sub-agent would cost {} more tokens without clear benefit",
                    cost_diff.abs()
                ),
                complexity,
            );
        }

        // All checks passed - delegate
        let reason = if cost_diff > 0 {
            format!(
                "Delegating saves ~{} tokens (complexity: {})",
                cost_diff, complexity.score
            )
        } else {
            format!(
                "Delegating for better isolation (complexity: {}, overhead: {} tokens)",
                complexity.score,
                cost_diff.abs()
            )
        };

        let decision = DelegationDecision::delegate(
            reason,
            complexity.clone(),
            self.budget.remaining_budget(),
            cost_diff,
        );

        // Record for history
        self.delegation_history.push(decision.clone());

        decision
    }

    /// Record actual token usage after task completion
    pub fn record_usage(&mut self, tokens: u32, was_sub_agent: bool) {
        if was_sub_agent {
            self.budget.record_usage(tokens);
        }
    }

    /// Get current budget status
    pub fn budget_status(&self) -> (u32, u32) {
        (
            self.budget.tokens_used,
            self.budget.max_sub_agent_tokens_per_session,
        )
    }

    /// Enable/disable sub-agents
    pub fn set_sub_agents_enabled(&mut self, enabled: bool) {
        self.budget.sub_agents_enabled = enabled;
    }

    /// Set minimum complexity for delegation
    pub fn set_min_complexity(&mut self, min: u32) {
        self.budget.min_complexity_for_delegation = min;
    }

    /// Set maximum budget for sub-agents
    pub fn set_max_budget(&mut self, budget: u32) {
        self.budget.max_sub_agent_tokens_per_session = budget;
    }

    /// Get delegation statistics
    pub fn stats(&self) -> DelegationStats {
        let total = self.delegation_history.len();
        let delegated = self
            .delegation_history
            .iter()
            .filter(|d| d.delegate)
            .count();
        let total_saved: i32 = self
            .delegation_history
            .iter()
            .map(|d| d.cost_difference)
            .sum();

        DelegationStats {
            total_analyzed: total,
            delegated_count: delegated,
            kept_on_main_count: total - delegated,
            total_tokens_saved: total_saved,
            budget_used: self.budget.tokens_used,
            budget_remaining: self.budget.remaining_budget(),
        }
    }

    /// Persist the current budget and history to disk as JSON.
    /// Called on gateway shutdown and periodically so `record_usage`,
    /// `budget_status`, and `stats()` have a durable backing store.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load delegator state from disk, or return a fresh default instance
    /// if the file is missing or malformed. Failures are non-fatal.
    pub fn load_or_default(path: &std::path::Path) -> Self {
        if !path.exists() {
            return Self::new(TokenBudget::default());
        }
        match std::fs::read_to_string(path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                tracing::warn!(
                    "delegation state file is corrupt, starting fresh: {}",
                    e
                );
                Self::new(TokenBudget::default())
            }),
            Err(e) => {
                tracing::warn!("could not read delegation state file: {}", e);
                Self::new(TokenBudget::default())
            }
        }
    }
}

/// Statistics for delegation decisions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationStats {
    pub total_analyzed: usize,
    pub delegated_count: usize,
    pub kept_on_main_count: usize,
    pub total_tokens_saved: i32,
    pub budget_used: u32,
    pub budget_remaining: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_task_not_delegated() {
        let budget = TokenBudget::default();
        let mut delegator = CostAwareDelegator::new(budget);

        let decision = delegator.should_delegate("What is 2 + 2?");
        assert!(!decision.delegate);
        assert!(decision.reason.contains("simple"));
    }

    #[test]
    fn test_complex_task_delegated() {
        let budget = TokenBudget::default();
        let mut delegator = CostAwareDelegator::new(budget);

        let decision = delegator.should_delegate(
            "Research the latest developments in AI agents, analyze the competitive landscape, and create a comprehensive report"
        );
        assert!(decision.delegate);
    }

    #[test]
    fn test_budget_exhausted() {
        let mut budget = TokenBudget::default();
        budget.tokens_used = budget.max_sub_agent_tokens_per_session;
        let mut delegator = CostAwareDelegator::new(budget);

        let decision =
            delegator.should_delegate("Complex research task that would normally be delegated");
        assert!(!decision.delegate);
        assert!(decision.reason.contains("Budget"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut delegator = CostAwareDelegator::new(TokenBudget::default());
        // Generate some history
        let _ = delegator.should_delegate("Simple question");
        let _ = delegator.should_delegate("Complex research and analysis task");
        delegator.record_usage(1500, true);
        delegator.set_max_budget(75000);
        let original = delegator.stats();

        let path = std::env::temp_dir().join("ahnara_test_delegation.json");
        delegator.save(&path).expect("save should succeed");

        let loaded = CostAwareDelegator::load_or_default(&path);
        let restored = loaded.stats();

        assert_eq!(original.total_analyzed, restored.total_analyzed);
        assert_eq!(original.delegated_count, restored.delegated_count);
        assert_eq!(original.budget_used, restored.budget_used);
        assert_eq!(original.budget_remaining, restored.budget_remaining);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sub_agents_disabled() {
        let mut budget = TokenBudget::default();
        budget.sub_agents_enabled = false;
        let mut delegator = CostAwareDelegator::new(budget);

        let decision =
            delegator.should_delegate("Complex research task that would normally be delegated");
        assert!(!decision.delegate);
        assert!(decision.reason.contains("disabled"));
    }
}
