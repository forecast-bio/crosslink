mod operations;

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::KnowledgeCommands;

pub use operations::*;

pub fn dispatch(command: KnowledgeCommands, crosslink_dir: &Path, global_json: bool) -> Result<()> {
    match command {
        KnowledgeCommands::Add {
            slug,
            title,
            tag,
            source,
            content,
            from_doc,
            repo,
        } => {
            reject_repo_on_write(repo.as_deref())?;
            add(
                crosslink_dir,
                &slug,
                title.as_deref(),
                &tag,
                &source,
                content.as_deref(),
                from_doc.as_deref(),
            )
        }
        KnowledgeCommands::Show {
            slug,
            repo,
            refresh,
        } => {
            if let Some(repo_value) = repo {
                crate::commands::external_knowledge::show(
                    crosslink_dir,
                    &repo_value,
                    &slug,
                    refresh,
                    global_json,
                    false,
                )
            } else {
                show(crosslink_dir, &slug, global_json)
            }
        }
        KnowledgeCommands::List {
            tag,
            contributor,
            since,
            json,
            repo,
            refresh,
        } => {
            if let Some(repo_value) = repo {
                crate::commands::external_knowledge::list(
                    crosslink_dir,
                    &repo_value,
                    tag.as_deref(),
                    contributor.as_deref(),
                    since.as_deref(),
                    refresh,
                    json,
                    false,
                )
            } else {
                list(
                    crosslink_dir,
                    tag.as_deref(),
                    contributor.as_deref(),
                    since.as_deref(),
                    json,
                )
            }
        }
        KnowledgeCommands::Edit {
            slug,
            append,
            content,
            replace_section,
            append_to_section,
            tag,
            source,
            from_doc,
            repo,
        } => {
            reject_repo_on_write(repo.as_deref())?;
            let effective_content = if let Some(ref doc_path) = from_doc {
                let doc_content = std::fs::read_to_string(doc_path)
                    .with_context(|| format!("Failed to read: {}", doc_path.display()))?;
                Some(doc_content)
            } else {
                content
            };
            edit(
                crosslink_dir,
                &slug,
                append.as_deref(),
                effective_content.as_deref(),
                replace_section.as_deref(),
                append_to_section.as_deref(),
                &tag,
                &source,
            )
        }
        KnowledgeCommands::Remove { slug, repo } => {
            reject_repo_on_write(repo.as_deref())?;
            remove(crosslink_dir, &slug)
        }
        KnowledgeCommands::Import {
            directory,
            tag,
            overwrite,
            dry_run,
            repo,
        } => {
            reject_repo_on_write(repo.as_deref())?;
            import(crosslink_dir, &directory, &tag, overwrite, dry_run)
        }
        KnowledgeCommands::Sync { repo } => {
            reject_repo_on_write(repo.as_deref())?;
            sync(crosslink_dir)
        }
        KnowledgeCommands::Search {
            query,
            context,
            source,
            tag,
            since,
            contributor,
            repo,
            refresh,
        } => {
            if let Some(repo_value) = repo {
                crate::commands::external_knowledge::search(
                    crosslink_dir,
                    &repo_value,
                    query.as_deref(),
                    context,
                    source.as_deref(),
                    refresh,
                    global_json,
                    false,
                    tag.as_deref(),
                    since.as_deref(),
                    contributor.as_deref(),
                )
            } else {
                search(
                    crosslink_dir,
                    query.as_deref(),
                    context,
                    source.as_deref(),
                    global_json,
                    tag.as_deref(),
                    since.as_deref(),
                    contributor.as_deref(),
                )
            }
        }
    }
}

/// Reject `--repo` on write commands with a clear error message.
fn reject_repo_on_write(repo: Option<&str>) -> Result<()> {
    if repo.is_some() {
        bail!(
            "External sources are read-only. The --repo flag is only supported on read commands."
        );
    }
    Ok(())
}
