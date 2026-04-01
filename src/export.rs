use anyhow::{Context, Result};
use std::path::Path;

use crate::connection::ImapSession;
use crate::display::MessageRow;
use crate::search;

/// Sanitize folder name for use in filenames: keep alphanumerics and hyphens, replace rest with `_`.
pub fn sanitize_folder_name(folder: &str) -> String {
    folder
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Export messages to .eml files. Returns (exported, skipped) counts.
pub fn export_messages(
    session: &mut ImapSession,
    messages: &[MessageRow],
    default_folder: &str,
    out_dir: &Path,
    force: bool,
) -> Result<(usize, usize)> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create directory '{}'", out_dir.display()))?;

    // Group by folder
    let mut by_folder: std::collections::HashMap<String, Vec<u32>> =
        std::collections::HashMap::new();
    for msg in messages {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| default_folder.to_string());
        by_folder.entry(folder).or_default().push(msg.uid);
    }

    let mut exported = 0usize;
    let mut skipped = 0usize;

    for (folder, uids) in &by_folder {
        session
            .select(folder)
            .with_context(|| format!("Failed to select '{folder}'"))?;

        let safe_folder = sanitize_folder_name(folder);

        for chunk in &search::build_uid_set(uids) {
            let fetches = session
                .uid_fetch(chunk, "BODY.PEEK[]")
                .with_context(|| format!("Failed to fetch messages from '{folder}'"))?;

            for fetch in fetches.iter() {
                let uid = match fetch.uid {
                    Some(u) => u,
                    None => continue,
                };
                if let Some(body) = fetch.body() {
                    let path = out_dir.join(format!("{safe_folder}_{uid}.eml"));
                    if path.exists() && !force {
                        skipped += 1;
                        continue;
                    }
                    std::fs::write(&path, body)
                        .with_context(|| format!("Failed to write '{}'", path.display()))?;
                    exported += 1;
                }
            }
        }
    }

    Ok((exported, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_folder_name_simple() {
        assert_eq!(sanitize_folder_name("INBOX"), "INBOX");
    }

    #[test]
    fn sanitize_folder_name_with_slash() {
        assert_eq!(sanitize_folder_name("[Gmail]/Trash"), "_Gmail__Trash");
    }

    #[test]
    fn sanitize_folder_name_with_spaces() {
        assert_eq!(sanitize_folder_name("Sent Items"), "Sent_Items");
    }

    #[test]
    fn sanitize_folder_name_preserves_hyphens() {
        assert_eq!(sanitize_folder_name("my-folder"), "my-folder");
    }

    #[test]
    fn sanitize_folder_name_empty() {
        assert_eq!(sanitize_folder_name(""), "");
    }

    #[test]
    fn sanitize_folder_name_dots_and_special() {
        assert_eq!(sanitize_folder_name("INBOX.Drafts"), "INBOX_Drafts");
        assert_eq!(sanitize_folder_name("Work/Projects"), "Work_Projects");
    }

    #[test]
    fn eml_filename_format() {
        let safe = sanitize_folder_name("[Gmail]/All Mail");
        let filename = format!("{safe}_{}.eml", 42);
        assert_eq!(filename, "_Gmail__All_Mail_42.eml");
    }
}
