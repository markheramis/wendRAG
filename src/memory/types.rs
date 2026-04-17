/*!
 * Core types for the memory subsystem.
 *
 * Defines data structures for memory entries, session messages,
 * query filters, and metadata.
 */

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/**
 * Scope of a memory entry - determines visibility and lifecycle.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryScope {
    /// Session-scoped: temporary, per-conversation memory.
    Session,
    /// User-scoped: persistent memory tied to a specific user.
    User,
    /// Global-scoped: shared across all users and sessions.
    Global,
}

impl MemoryScope {
    /// Returns the string representation of the scope.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryScope::Session => "session",
            MemoryScope::User => "user",
            MemoryScope::Global => "global",
        }
    }
}

/**
 * Type of memory entry - categorizes the content.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    /// A factual statement extracted from conversation.
    Fact,
    /// A user preference or setting.
    Preference,
    /// An event that occurred.
    Event,
    /// A summary of a session or conversation.
    Summary,
    /// A chat message.
    Message,
}

impl MemoryType {
    /// Returns the string representation of the memory type.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryType::Fact => "fact",
            MemoryType::Preference => "preference",
            MemoryType::Event => "event",
            MemoryType::Summary => "summary",
            MemoryType::Message => "message",
        }
    }
}

/**
 * Role of a participant in a chat message.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    /// The user/system sending the message.
    User,
    /// The AI assistant responding.
    Assistant,
    /// System message or context.
    System,
}

impl MessageRole {
    /// Returns the string representation of the role.
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        }
    }
}

/**
 * Metadata associated with a memory entry.
 */
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    /// Type of memory entry.
    pub entry_type: MemoryType,
    /// Source system or component that created the entry.
    pub source: String,
    /// Optional TTL in seconds for automatic expiration.
    pub ttl_seconds: Option<i64>,
    /// Optional reference to another memory this consolidates into.
    pub consolidation_target: Option<Uuid>,
    /// Additional arbitrary metadata as JSON.
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl Default for MemoryMetadata {
    fn default() -> Self {
        Self {
            entry_type: MemoryType::Fact,
            source: "memory_system".to_string(),
            ttl_seconds: None,
            consolidation_target: None,
            extra: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/**
 * A memory entry - the core unit of storage in the memory system.
 */
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique identifier for the entry.
    pub id: Uuid,
    /// Scope determining visibility.
    pub scope: MemoryScope,
    /// Optional session ID for session-scoped entries.
    pub session_id: Option<String>,
    /// Optional user ID for user-scoped entries.
    pub user_id: Option<String>,
    /// Content of the memory.
    pub content: String,
    /// Importance score (0.0 - 1.0) affecting retention priority.
    pub importance_score: f32,
    /// When the entry was created.
    pub created_at: DateTime<Utc>,
    /// When the entry was last accessed.
    pub last_accessed: DateTime<Utc>,
    /// Number of times the entry has been accessed.
    pub access_count: u32,
    /// Optional embedding vector for semantic search.
    pub embedding: Option<Vec<f32>>,
    /// Metadata for the entry.
    pub metadata: MemoryMetadata,
}

impl MemoryEntry {
    /**
     * Create a new memory entry.
     *
     * Parameters:
     * - `scope`: The visibility scope.
     * - `session_id`: Optional session identifier.
     * - `user_id`: Optional user identifier.
     * - `content`: The memory content.
     * - `entry_type`: Type of memory.
     */
    pub fn new(
        scope: MemoryScope,
        session_id: Option<String>,
        user_id: Option<String>,
        content: impl Into<String>,
        entry_type: MemoryType,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            scope,
            session_id,
            user_id,
            content: content.into(),
            importance_score: 0.5,
            created_at: now,
            last_accessed: now,
            access_count: 0,
            embedding: None,
            metadata: MemoryMetadata {
                entry_type,
                ..Default::default()
            },
        }
    }
}

/**
 * A chat message within a session.
 */
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role of the message sender.
    pub role: MessageRole,
    /// Content of the message.
    pub content: String,
    /// Timestamp when the message was sent.
    pub timestamp: DateTime<Utc>,
    /// Optional metadata.
    pub metadata: Option<serde_json::Value>,
}

impl ChatMessage {
    /**
     * Create a new chat message.
     *
     * Parameters:
     * - `role`: The sender role.
     * - `content`: The message content.
     */
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            timestamp: Utc::now(),
            metadata: None,
        }
    }

    /**
     * Create a new user message (convenience constructor).
     */
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(MessageRole::User, content)
    }

    /**
     * Create a new assistant message (convenience constructor).
     */
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(MessageRole::Assistant, content)
    }
}

/**
 * Query parameters for searching memories.
 */
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    /// Filter by scope.
    pub scope: Option<MemoryScope>,
    /// Filter by session ID.
    pub session_id: Option<String>,
    /// Filter by user ID.
    pub user_id: Option<String>,
    /// Text query for semantic search.
    pub text: Option<String>,
    /// Optional embedding for vector search.
    pub embedding: Option<Vec<f32>>,
    /// Maximum results to return.
    pub limit: usize,
    /// Filter by minimum importance score.
    pub min_importance: Option<f32>,
    /// Filter by entry type.
    pub entry_type: Option<MemoryType>,
}

impl MemoryQuery {
    /**
     * Create a new empty query.
     */
    pub fn new() -> Self {
        Self {
            limit: 10,
            ..Default::default()
        }
    }

    /**
     * Set the scope filter.
     */
    pub fn scope(mut self, scope: MemoryScope) -> Self {
        self.scope = Some(scope);
        self
    }

    /**
     * Set the session ID filter.
     */
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /**
     * Set the user ID filter.
     */
    pub fn user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /**
     * Set the text query for semantic search.
     */
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /**
     * Set the embedding for vector search.
     */
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /**
     * Set the result limit.
     */
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /**
     * Set the user ID filter (convenience method).
     */
    pub fn for_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_scope_as_str() {
        assert_eq!(MemoryScope::Session.as_str(), "session");
        assert_eq!(MemoryScope::User.as_str(), "user");
        assert_eq!(MemoryScope::Global.as_str(), "global");
    }

    #[test]
    fn test_memory_type_as_str() {
        assert_eq!(MemoryType::Fact.as_str(), "fact");
        assert_eq!(MemoryType::Preference.as_str(), "preference");
        assert_eq!(MemoryType::Event.as_str(), "event");
        assert_eq!(MemoryType::Summary.as_str(), "summary");
        assert_eq!(MemoryType::Message.as_str(), "message");
    }

    #[test]
    fn test_message_role_as_str() {
        assert_eq!(MessageRole::User.as_str(), "user");
        assert_eq!(MessageRole::Assistant.as_str(), "assistant");
        assert_eq!(MessageRole::System.as_str(), "system");
    }

    #[test]
    fn test_memory_entry_creation() {
        let entry = MemoryEntry::new(
            MemoryScope::User,
            None,
            Some("user123".to_string()),
            "Test content",
            MemoryType::Fact,
        );

        assert_eq!(entry.scope, MemoryScope::User);
        assert_eq!(entry.user_id, Some("user123".to_string()));
        assert_eq!(entry.content, "Test content");
        assert_eq!(entry.metadata.entry_type, MemoryType::Fact);
        assert_eq!(entry.importance_score, 0.5);
    }

    #[test]
    fn test_chat_message_creation() {
        let msg = ChatMessage::new(MessageRole::User, "Hello");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn test_memory_query_builder() {
        let query = MemoryQuery::new()
            .scope(MemoryScope::Session)
            .session_id("session123")
            .limit(20);

        assert_eq!(query.scope, Some(MemoryScope::Session));
        assert_eq!(query.session_id, Some("session123".to_string()));
        assert_eq!(query.limit, 20);
    }
}
