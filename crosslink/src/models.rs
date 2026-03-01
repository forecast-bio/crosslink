use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub parent_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub active_issue_id: Option<i64>,
    pub handoff_notes: Option<String>,
    pub last_action: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Milestone {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
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
            status: "open".to_string(),
            priority: "high".to_string(),
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
            status: "open".to_string(),
            priority: "medium".to_string(),
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
            status: "closed".to_string(),
            priority: "low".to_string(),
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
            status: "open".to_string(),
            priority: "high".to_string(),
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
            status: "open".to_string(),
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
            status: "closed".to_string(),
            created_at: now,
            closed_at: Some(now),
        };

        let json = serde_json::to_string(&milestone).unwrap();
        let deserialized: Milestone = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.closed_at, Some(now));
        assert_eq!(deserialized.status, "closed");
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_issue_json_roundtrip(
            id in 1i64..10000,
            title in "[a-zA-Z0-9 ]{1,100}",
            status in "open|closed",
            priority in "low|medium|high|critical"
        ) {
            let issue = Issue {
                id,
                title: title.clone(),
                description: None,
                status: status.clone(),
                priority: priority.clone(),
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
            status in "open|closed"
        ) {
            let milestone = Milestone {
                id,
                name: name.clone(),
                description: None,
                status: status.clone(),
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
                status: if is_closed { "closed".to_string() } else { "open".to_string() },
                priority: "medium".to_string(),
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
