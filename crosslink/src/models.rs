use chrono::{DateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Issue lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueStatus {
    Open,
    Closed,
    Archived,
}

impl fmt::Display for IssueStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Closed => write!(f, "closed"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

impl FromStr for IssueStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            "archived" => Ok(Self::Archived),
            other => {
                anyhow::bail!("Invalid status '{other}'. Valid values: open, closed, archived")
            }
        }
    }
}

impl IssueStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Archived => "archived",
        }
    }
}

/// Issue priority level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

impl FromStr for Priority {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            other => anyhow::bail!(
                "Invalid priority '{other}'. Valid values: low, medium, high, critical"
            ),
        }
    }
}

impl Priority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

// Enable comparison with string types for ergonomic use in display code.

impl PartialEq<str> for IssueStatus {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for IssueStatus {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<IssueStatus> for str {
    fn eq(&self, other: &IssueStatus) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<IssueStatus> for &str {
    fn eq(&self, other: &IssueStatus) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for IssueStatus {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<IssueStatus> for String {
    fn eq(&self, other: &IssueStatus) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for Priority {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Priority {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Priority> for str {
    fn eq(&self, other: &Priority) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Priority> for &str {
    fn eq(&self, other: &Priority) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<String> for Priority {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Priority> for String {
    fn eq(&self, other: &Priority) -> bool {
        self.as_str() == other.as_str()
    }
}

// rusqlite integration — store as text in SQLite, parse on read.

impl ToSql for IssueStatus {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for IssueStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e: anyhow::Error| FromSqlError::Other(e.into()))
    }
}

impl ToSql for Priority {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Priority {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e: anyhow::Error| FromSqlError::Other(e.into()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: IssueStatus,
    pub priority: Priority,
    pub parent_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Comment {
    pub id: i64,
    pub issue_id: i64,
    pub content: String,
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_comment_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intervention_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_key_fingerprint: Option<String>,
}

fn default_comment_kind() -> String {
    "note".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub active_issue_id: Option<i64>,
    pub handoff_notes: Option<String>,
    pub last_action: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Milestone {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: IssueStatus,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenUsage {
    pub id: i64,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    pub timestamp: DateTime<Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<i64>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ==================== Issue Tests ====================

    #[test]
    fn test_issue_serialization_json() {
        let issue = Issue {
            id: 1,
            title: "Test issue".to_string(),
            description: Some("A description".to_string()),
            status: IssueStatus::Open,
            priority: Priority::High,
            parent_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        };

        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(issue.id, deserialized.id);
        assert_eq!(issue.title, deserialized.title);
        assert_eq!(issue.description, deserialized.description);
        assert_eq!(issue.status, deserialized.status);
        assert_eq!(issue.priority, deserialized.priority);
        assert_eq!(issue.parent_id, deserialized.parent_id);
    }

    #[test]
    fn test_issue_with_parent() {
        let issue = Issue {
            id: 2,
            title: "Child issue".to_string(),
            description: None,
            status: IssueStatus::Open,
            priority: Priority::Medium,
            parent_id: Some(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        };

        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.parent_id, Some(1));
    }

    #[test]
    fn test_issue_closed_at() {
        let now = Utc::now();
        let issue = Issue {
            id: 1,
            title: "Closed issue".to_string(),
            description: None,
            status: IssueStatus::Closed,
            priority: Priority::Low,
            parent_id: None,
            created_at: now,
            updated_at: now,
            closed_at: Some(now),
        };

        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.closed_at, Some(now));
    }

    #[test]
    fn test_issue_unicode_fields() {
        let issue = Issue {
            id: 1,
            title: "测试 🐛 αβγ".to_string(),
            description: Some("Description with émojis 🎉".to_string()),
            status: IssueStatus::Open,
            priority: Priority::High,
            parent_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
        };

        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.title, "测试 🐛 αβγ");
        assert_eq!(
            deserialized.description,
            Some("Description with émojis 🎉".to_string())
        );
    }

    // ==================== Comment Tests ====================

    #[test]
    fn test_comment_serialization() {
        let comment = Comment {
            id: 1,
            issue_id: 42,
            content: "A comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
        };

        let json = serde_json::to_string(&comment).unwrap();
        let deserialized: Comment = serde_json::from_str(&json).unwrap();

        assert_eq!(comment.id, deserialized.id);
        assert_eq!(comment.issue_id, deserialized.issue_id);
        assert_eq!(comment.content, deserialized.content);
    }

    #[test]
    fn test_comment_empty_content() {
        let comment = Comment {
            id: 1,
            issue_id: 1,
            content: "".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
        };

        let json = serde_json::to_string(&comment).unwrap();
        let deserialized: Comment = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.content, "");
    }

    #[test]
    fn test_comment_default_kind_when_missing() {
        // When `kind` is absent from JSON, the serde default should provide "note"
        let json = serde_json::json!({
            "id": 1,
            "issue_id": 2,
            "content": "hello",
            "created_at": "2026-01-01T00:00:00Z"
        });
        let comment: Comment = serde_json::from_value(json).unwrap();
        assert_eq!(comment.kind, "note");
    }

    // ==================== Session Tests ====================

    #[test]
    fn test_session_serialization() {
        let session = Session {
            id: 1,
            started_at: Utc::now(),
            ended_at: None,
            active_issue_id: Some(5),
            handoff_notes: Some("Notes here".to_string()),
            last_action: None,
            agent_id: None,
        };

        let json = serde_json::to_string(&session).unwrap();
        let deserialized: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(session.id, deserialized.id);
        assert_eq!(session.active_issue_id, deserialized.active_issue_id);
        assert_eq!(session.handoff_notes, deserialized.handoff_notes);
    }

    #[test]
    fn test_session_ended() {
        let now = Utc::now();
        let session = Session {
            id: 1,
            started_at: now,
            ended_at: Some(now),
            active_issue_id: None,
            handoff_notes: Some("Final notes".to_string()),
            last_action: None,
            agent_id: None,
        };

        let json = serde_json::to_string(&session).unwrap();
        let deserialized: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.ended_at, Some(now));
        assert_eq!(deserialized.handoff_notes, Some("Final notes".to_string()));
    }

    // ==================== Milestone Tests ====================

    #[test]
    fn test_milestone_serialization() {
        let milestone = Milestone {
            id: 1,
            name: "v1.0".to_string(),
            description: Some("First release".to_string()),
            status: IssueStatus::Open,
            created_at: Utc::now(),
            closed_at: None,
        };

        let json = serde_json::to_string(&milestone).unwrap();
        let deserialized: Milestone = serde_json::from_str(&json).unwrap();

        assert_eq!(milestone.id, deserialized.id);
        assert_eq!(milestone.name, deserialized.name);
        assert_eq!(milestone.description, deserialized.description);
        assert_eq!(milestone.status, deserialized.status);
    }

    #[test]
    fn test_milestone_closed() {
        let now = Utc::now();
        let milestone = Milestone {
            id: 1,
            name: "v1.0".to_string(),
            description: None,
            status: IssueStatus::Closed,
            created_at: now,
            closed_at: Some(now),
        };

        let json = serde_json::to_string(&milestone).unwrap();
        let deserialized: Milestone = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.closed_at, Some(now));
        assert_eq!(deserialized.status, IssueStatus::Closed);
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_issue_json_roundtrip(
            id in 1i64..10000,
            title in "[a-zA-Z0-9 ]{1,100}",
            is_closed in proptest::bool::ANY,
            prio_idx in 0usize..4,
        ) {
            let status = if is_closed { IssueStatus::Closed } else { IssueStatus::Open };
            let priority = [Priority::Low, Priority::Medium, Priority::High, Priority::Critical][prio_idx];
            let issue = Issue {
                id,
                title: title.clone(),
                description: None,
                status,
                priority,
                parent_id: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                closed_at: None,
            };

            let json = serde_json::to_string(&issue).unwrap();
            let deserialized: Issue = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(deserialized.id, id);
            prop_assert_eq!(deserialized.title, title);
            prop_assert_eq!(deserialized.status, status);
            prop_assert_eq!(deserialized.priority, priority);
        }

        #[test]
        fn prop_comment_json_roundtrip(
            id in 1i64..10000,
            issue_id in 1i64..10000,
            content in "[a-zA-Z0-9 ]{0,500}"
        ) {
            let comment = Comment {
                id,
                issue_id,
                content: content.clone(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
            };

            let json = serde_json::to_string(&comment).unwrap();
            let deserialized: Comment = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(deserialized.id, id);
            prop_assert_eq!(deserialized.issue_id, issue_id);
            prop_assert_eq!(deserialized.content, content);
        }

        #[test]
        fn prop_session_json_roundtrip(
            id in 1i64..10000,
            active_issue_id in prop::option::of(1i64..10000),
            handoff_notes in prop::option::of("[a-zA-Z0-9 ]{0,200}")
        ) {
            let session = Session {
                id,
                started_at: Utc::now(),
                ended_at: None,
                active_issue_id,
                handoff_notes: handoff_notes.clone(),
                last_action: None,
                agent_id: None,
            };

            let json = serde_json::to_string(&session).unwrap();
            let deserialized: Session = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(deserialized.id, id);
            prop_assert_eq!(deserialized.active_issue_id, active_issue_id);
            prop_assert_eq!(deserialized.handoff_notes, handoff_notes);
        }

        #[test]
        fn prop_milestone_json_roundtrip(
            id in 1i64..10000,
            name in "[a-zA-Z0-9.]{1,50}",
            is_closed in proptest::bool::ANY,
        ) {
            let status = if is_closed { IssueStatus::Closed } else { IssueStatus::Open };
            let milestone = Milestone {
                id,
                name: name.clone(),
                description: None,
                status,
                created_at: Utc::now(),
                closed_at: None,
            };

            let json = serde_json::to_string(&milestone).unwrap();
            let deserialized: Milestone = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(deserialized.id, id);
            prop_assert_eq!(deserialized.name, name);
            prop_assert_eq!(deserialized.status, status);
        }

        #[test]
        fn prop_issue_with_optional_fields(
            has_desc in proptest::bool::ANY,
            has_parent in proptest::bool::ANY,
            is_closed in proptest::bool::ANY
        ) {
            let now = Utc::now();
            let issue = Issue {
                id: 1,
                title: "Test".to_string(),
                description: if has_desc { Some("Desc".to_string()) } else { None },
                status: if is_closed { IssueStatus::Closed } else { IssueStatus::Open },
                priority: Priority::Medium,
                parent_id: if has_parent { Some(99) } else { None },
                created_at: now,
                updated_at: now,
                closed_at: if is_closed { Some(now) } else { None },
            };

            let json = serde_json::to_string(&issue).unwrap();
            let deserialized: Issue = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(deserialized.description.is_some(), has_desc);
            prop_assert_eq!(deserialized.parent_id.is_some(), has_parent);
            prop_assert_eq!(deserialized.closed_at.is_some(), is_closed);
        }
    }
}
