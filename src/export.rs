use anyhow::{Context, Result};
use std::path::Path;

use crate::connection::ImapSession;
use crate::display::MessageRow;
use crate::search;

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
                    let path = out_dir.join(format!("{uid}.eml"));
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
