//! Structured Error Recovery Paths
//! Provides typed errors with recovery strategies

/// Agent error categories with recovery hints
#[derive(Debug, Clone)]
pub enum AgentError {
    /// Provider failed - try fallback or retry
    ProviderError {
        provider: String,
        message: String,
        retryable: bool,
        suggested_action: RecoveryAction,
    },
    /// Tool execution failed
    ToolError {
        tool: String,
        message: String,
        context: String,
        suggested_action: RecoveryAction,
    },
    /// Session/context error
    SessionError {
        session_id: String,
        message: String,
        suggested_action: RecoveryAction,
    },
    /// Rate limit hit
    RateLimitError {
        provider: String,
        retry_after_secs: u64,
    },
    /// Context too long
    ContextOverflow {
        tokens: u64,
        limit: u64,
        suggested_action: RecoveryAction,
    },
    /// Sub-agent failure
    SubAgentError {
        agent_id: String,
        task: String,
        error: String,
    },
    /// Timeout exceeded
    TimeoutError {
        operation: String,
        duration_secs: u64,
    },
}

/// Recovery actions the system can take
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Retry with exponential backoff
    RetryWithBackoff { max_attempts: u32, base_delay_ms: u64 },
    /// Switch to fallback provider
    SwitchToFallback { fallback_name: String },
    /// Truncate context and retry
    TruncateContext { keep_last_n: usize },
    /// Graceful degradation - return partial result
    DegradedResponse { message: String },
    /// Notify user and wait for input
    RequestUserInput { prompt: String },
    /// No automatic recovery possible
    NoRecovery { reason: String },
    /// Restart the session
    RestartSession,
    /// Spawn sub-agent to handle
    DelegateToSubAgent { agent_type: String },
}


impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.user_message())
    }
}

impl std::error::Error for AgentError {}

impl AgentError {
    /// Convert to user-friendly message
    pub fn user_message(&self) -> String {
        match self {
            Self::ProviderError { provider, message, .. } => {
                format!("Connection issue with {}. Retrying...", provider)
            }
            Self::ToolError { tool, .. } => {
                format!("Tool '{}' encountered an issue. Trying alternative approach...", tool)
            }
            Self::SessionError { .. } => {
                "Session issue detected. Starting fresh...".to_string()
            }
            Self::RateLimitError { retry_after_secs, .. } => {
                format!("Rate limit reached. Waiting {} seconds...", retry_after_secs)
            }
            Self::ContextOverflow { .. } => {
                "Context is getting long. Summarizing earlier conversation...".to_string()
            }
            Self::SubAgentError { agent_id, .. } => {
                format!("Sub-agent {} encountered an issue. Retrying...", agent_id)
            }
            Self::TimeoutError { operation, .. } => {
                format!("{} is taking longer than expected. Please wait...", operation)
            }
        }
    }

    /// Check if error is recoverable
    pub fn is_recoverable(&self) -> bool {
        !matches!(
            self.suggested_action(),
            RecoveryAction::NoRecovery { .. }
        )
    }

    /// Get suggested recovery action
    pub fn suggested_action(&self) -> RecoveryAction {
        match self {
            Self::ProviderError { suggested_action, .. } => suggested_action.clone(),
            Self::ToolError { suggested_action, .. } => suggested_action.clone(),
            Self::SessionError { suggested_action, .. } => suggested_action.clone(),
            Self::RateLimitError { retry_after_secs, .. } => {
                RecoveryAction::RetryWithBackoff {
                    max_attempts: 1,
                    base_delay_ms: retry_after_secs * 1000,
                }
            }
            Self::ContextOverflow { .. } => {
                RecoveryAction::TruncateContext { keep_last_n: 20 }
            }
            Self::SubAgentError { .. } => {
                RecoveryAction::RetryWithBackoff {
                    max_attempts: 2,
                    base_delay_ms: 1000,
                }
            }
            Self::TimeoutError { .. } => {
                RecoveryAction::RetryWithBackoff {
                    max_attempts: 1,
                    base_delay_ms: 2000,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_error_recovery() {
        let error = AgentError::ProviderError {
            provider: "nvidia".to_string(),
            message: "Connection timeout".to_string(),
            retryable: true,
            suggested_action: RecoveryAction::RetryWithBackoff {
                max_attempts: 3,
                base_delay_ms: 1000,
            },
        };
        
        assert!(error.is_recoverable());
        assert!(error.user_message().contains("nvidia"));
    }
}
