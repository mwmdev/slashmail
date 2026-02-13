use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

use crate::connection::ImapSession;
use crate::display::display_messages;
use crate::search::{self, SearchCriteria};

fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Check that a destination folder exists on the server.
fn ensure_folder_exists(session: &mut ImapSession, folder: &str) -> Result<()> {
    let folders = session
        .list(Some(""), Some("*"))
        .context("Failed to list folders")?;
    let exists = folders.iter().any(|f| f.name() == folder);
    if !exists {
        bail!(
            "Folder '{folder}' does not exist. Use `slashmail status` to list available folders."
        );
    }
    Ok(())
}

pub fn search_and_move(
    session: &mut ImapSession,
    criteria: &SearchCriteria,
    dest: &str,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    let sp = spinner("Searching...");
    let messages = search::search(session, criteria)?;
    sp.finish_and_clear();

    if messages.is_empty() {
        println!("No messages match the criteria.");
        return Ok(());
    }

    display_messages(&messages);

    if dry_run {
        println!(
            "Dry run: {} message(s) would be moved to {dest}.",
            messages.len()
        );
        return Ok(());
    }

    ensure_folder_exists(session, dest)?;

    if !yes {
        let confirm =
            inquire::Confirm::new(&format!("Move {} message(s) to {dest}?", messages.len()))
                .with_default(false)
                .prompt()
                .context("Prompt failed")?;

        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let sp = spinner(&format!("Moving to {dest}..."));

    // Group by folder for multi-folder moves
    let mut by_folder: std::collections::HashMap<String, Vec<u32>> =
        std::collections::HashMap::new();
    for msg in &messages {
        let folder = msg
            .folder
            .clone()
            .unwrap_or_else(|| criteria.folder.clone());
        by_folder.entry(folder).or_default().push(msg.uid);
    }

    let mut total = 0usize;
    for (folder, uids) in &by_folder {
        session
            .select(folder)
            .with_context(|| format!("Failed to select '{folder}'"))?;

        for chunk in &search::build_uid_set(uids) {
            session
                .uid_move_or_fallback(chunk, dest)
                .with_context(|| format!("Failed to move messages from '{folder}' to {dest}"))?;
        }

        total += uids.len();
    }

    sp.finish_and_clear();
    println!("Moved {total} message(s) to {dest}.");
    Ok(())
}

pub fn delete(
    session: &mut ImapSession,
    criteria: &SearchCriteria,
    trash_folder: &str,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    search_and_move(session, criteria, trash_folder, yes, dry_run)
}
