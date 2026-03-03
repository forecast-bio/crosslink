#![no_main]

//! Fuzz target for comment operations.
//!
//! Tests add_comment, get_comments, update_comment_content with arbitrary
//! Unicode content and comment kinds.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug)]
struct CommentInput {
    content: String,
    kind: String,
    update_content: Option<String>,
    num_comments: u8,
}

fuzz_target!(|input: CommentInput| {
    let dir = match tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let db_path = dir.path().join("issues.db");

    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let issue_id = match db.create_issue("Fuzz test issue", None, "medium") {
        Ok(id) => id,
        Err(_) => return,
    };

    // Fuzz adding comments with arbitrary content and kinds
    let num = (input.num_comments % 10).max(1);
    let mut comment_ids = Vec::new();
    for _ in 0..num {
        if let Ok(id) = db.add_comment(issue_id, &input.content, &input.kind) {
            comment_ids.push(id);
        }
    }

    // Fuzz retrieving comments
    let _ = db.get_comments(issue_id);

    // Fuzz updating comment content
    if let Some(new_content) = &input.update_content {
        for cid in &comment_ids {
            let _ = db.update_comment_content(*cid, new_content);
        }
    }

    // Verify retrieval after updates
    let _ = db.get_comments(issue_id);

    // Fuzz with nonexistent issue IDs
    let _ = db.add_comment(999999, &input.content, &input.kind);
    let _ = db.get_comments(999999);
});
