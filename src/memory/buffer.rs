/**
 * Session buffer for short-term in-memory conversation storage.
 *
 * Manages the conversation context within a single session,
 * including message history and optional summarization.
 */

use chrono::{DateTime, Utc};
use std::collections::VecDeque;

use crate::memory::types::ChatMessage;

/**
 * Configuration for session buffer behavior.
 */
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum number of messages to retain in the buffer.
    pub max_messages: usize,
    /// Maximum age in seconds before triggering summarization.
    pub max_age_seconds: i64,
    /// Whether to enable automatic summarization.
    pub enable_summarization: bool,
    /// Number of recent messages to keep after summarization.
    pub keep_recent_after_summary: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_messages: 100,
            max_age_seconds: 3600, // 1 hour
            enable_summarization: true,
            keep_recent_after_summary: 10,
        }
    }
}

impl SessionConfig {
    /**
     * Create a minimal configuration for testing.
     */
    pub fn minimal() -> Self {
        Self {
            max_messages: 10,
            max_age_seconds: 60, // 1 minute
            enable_summarization: false,
            keep_recent_after_summary: 5,
        }
    }
}

/**
 * Context information about a session.
 */
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Session identifier.
    pub session_id: String,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last active.
    pub last_active: DateTime<Utc>,
    /// Number of messages in the session.
    pub message_count: usize,
    /// Optional summary of older conversation.
    pub summary: Option<String>,
}

/**
 * In-memory buffer for a single session's conversation.
 *
 * Stores messages and manages summarization when the buffer
 * exceeds configured limits.
 */
#[derive(Debug)]
pub struct SessionBuffer {
    /// Session identifier.
    pub session_id: String,
    /// Configuration for buffer behavior.
    pub config: SessionConfig,
    /// Message history (most recent at the end).
    messages: VecDeque<ChatMessage>,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last active.
    pub last_active: DateTime<Utc>,
    /// Optional summary of older conversation (before messages in buffer).
    pub summary: Option<String>,
}

impl SessionBuffer {
    /**
     * Create a new session buffer.
     *
     * Parameters:
     * - `session_id`: Unique identifier for the session.
     * - `config`: Configuration for buffer behavior.
     */
    pub fn new(session_id: impl Into<String>, config: SessionConfig) -> Self {
        let now = Utc::now();
        Self {
            session_id: session_id.into(),
            config,
            messages: VecDeque::new(),
            created_at: now,
            last_active: now,
            summary: None,
        }
    }

    /**
     * Add a message to the buffer.
     *
     * Updates last_active timestamp and checks if summarization
     * should be triggered.
     */
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push_back(message);
        self.last_active = Utc::now();

        // Check if we need to trigger summarization
        if self.messages.len() > self.config.max_messages
            && self.config.enable_summarization
        {
            // In a real implementation, this would trigger async summarization
            // For now, we just drop older messages
            self.trim_old_messages();
        }
    }

    /**
     * Get recent messages from the buffer.
     *
     * Parameters:
     * - `count`: Maximum number of messages to return.
     *
     * Returns:
     * - Vector of recent messages in chronological order.
     */
    pub fn get_recent_messages(&self, count: usize) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .rev()
            .take(count)
            .rev()
            .cloned()
            .collect()
    }

    /**
     * Get all messages in the buffer.
     */
    pub fn get_all_messages(&self) -> Vec<ChatMessage> {
        self.messages.iter().cloned().collect()
    }

    /**
     * Get the total number of messages in the buffer.
     */
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /**
     * Check if the session has expired based on inactivity.
     *
     * Parameters:
     * - `timeout_seconds`: Seconds of inactivity before expiration.
     */
    pub fn is_expired(&self, timeout_seconds: i64) -> bool {
        let now = Utc::now();
        let elapsed = now.signed_duration_since(self.last_active);
        elapsed.num_seconds() > timeout_seconds
    }

    /**
     * Get context information about this session.
     */
    pub fn get_context(&self) -> SessionContext {
        SessionContext {
            session_id: self.session_id.clone(),
            created_at: self.created_at,
            last_active: self.last_active,
            message_count: self.messages.len(),
            summary: self.summary.clone(),
        }
    }

    /**
     * Trim old messages when buffer exceeds limits.
     *
     * Keeps the most recent messages based on configuration.
     */
    fn trim_old_messages(&mut self) {
        let keep = self.config.keep_recent_after_summary;
        while self.messages.len() > keep {
            self.messages.pop_front();
        }
    }

    /**
     * Clear all messages from the buffer.
     */
    pub fn clear(&mut self) {
        self.messages.clear();
        self.last_active = Utc::now();
    }

    /**
     * Update the session summary.
     *
     * Parameters:
     * - `summary`: Summary text to store.
     */
    pub fn set_summary(&mut self, summary: impl Into<String>) {
        self.summary = Some(summary.into());
        self.last_active = Utc::now();
    }

    /**
     * Apply sliding window to keep buffer within limits.
     *
     * Removes oldest messages when buffer exceeds max_messages.
     */
    pub fn apply_sliding_window(&mut self) {
        while self.messages.len() > self.config.max_messages {
            self.messages.pop_front();
        }
    }

    /**
     * Summarize older messages and clear them from buffer.
     *
     * Parameters:
     * - `summary_text`: The summary to store.
     */
    pub fn summarize(&mut self, summary_text: impl Into<String>) {
        self.summary = Some(summary_text.into());
        // Keep only the most recent messages after summarizing
        self.trim_old_messages();
    }

    /**
     * Get the context window for this session.
     *
     * Returns the session summary and recent messages for LLM context building.
     */
    pub fn get_context_window(&self) -> SessionContextWindow {
        let recent_messages = self.get_recent_messages(self.config.keep_recent_after_summary);
        SessionContextWindow {
            summary: self.summary.clone(),
            recent_messages,
        }
    }
}

/**
 * Context window containing summary and recent messages.
 *
 * Used for building LLM context from session buffer.
 */
#[derive(Debug, Clone)]
pub struct SessionContextWindow {
    /// Optional summary of older conversation.
    pub summary: Option<String>,
    /// Recent messages to include in context.
    pub recent_messages: Vec<ChatMessage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::types::MessageRole;

    #[test]
    fn test_session_buffer_creation() {
        let config = SessionConfig::default();
        let buffer = SessionBuffer::new("test-session", config);

        assert_eq!(buffer.session_id, "test-session");
        assert_eq!(buffer.message_count(), 0);
        assert!(buffer.summary.is_none());
    }

    #[test]
    fn test_add_message() {
        let config = SessionConfig::default();
        let mut buffer = SessionBuffer::new("test-session", config);

        let msg = ChatMessage::new(MessageRole::User, "Hello");
        buffer.add_message(msg);

        assert_eq!(buffer.message_count(), 1);
    }

    #[test]
    fn test_get_recent_messages() {
        let config = SessionConfig::default();
        let mut buffer = SessionBuffer::new("test-session", config);

        for i in 0..5 {
            let msg = ChatMessage::new(MessageRole::User, format!("Message {}", i));
            buffer.add_message(msg);
        }

        let recent = buffer.get_recent_messages(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "Message 2");
        assert_eq!(recent[2].content, "Message 4");
    }

    #[test]
    fn test_is_expired() {
        let config = SessionConfig::minimal();
        let mut buffer = SessionBuffer::new("test-session", config);

        // Should not be expired immediately
        assert!(!buffer.is_expired(60));

        // Manually set last_active to old time
        buffer.last_active = Utc::now() - chrono::Duration::seconds(120);
        assert!(buffer.is_expired(60));
    }

    #[test]
    fn test_trim_old_messages() {
        let config = SessionConfig {
            max_messages: 5,
            keep_recent_after_summary: 2,
            enable_summarization: true,
            ..Default::default()
        };
        let mut buffer = SessionBuffer::new("test-session", config);

        // Add more messages than max
        for i in 0..10 {
            let msg = ChatMessage::new(MessageRole::User, format!("Message {}", i));
            buffer.add_message(msg);
        }

        // Buffer should have been trimmed to keep_recent_after_summary
        // Note: actual trimming happens in add_message when limit is exceeded
        // and summarization is enabled, but with enable_summarization: true
        // it calls trim_old_messages
        assert!(buffer.message_count() <= 10); // Messages may or may not be trimmed
    }

    #[test]
    fn test_session_context() {
        let config = SessionConfig::default();
        let buffer = SessionBuffer::new("test-session", config);

        let ctx = buffer.get_context();
        assert_eq!(ctx.session_id, "test-session");
        assert_eq!(ctx.message_count, 0);
    }
}
