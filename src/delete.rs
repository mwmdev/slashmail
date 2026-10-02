use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

use crate::connection::ImapSession;
use crate::display::{display_messages, sanitize_folder_name, sanitize_terminal_field};
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

pub fn search_and_move(
    session: &mut ImapSession,
    criteria: &mut SearchCriteria,
    dest: &str,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    search_and_move_with_account(session, criteria, dest, yes, dry_run, None)
}

pub fn search_and_move_with_account(
    session: &mut ImapSession,
    criteria: &mut SearchCriteria,
    dest: &str,
    yes: bool,
    dry_run: bool,
    account_name: Option<&str>,
) -> Result<()> {
    // Resolve the destination first so it is excluded from the search. A
    // missing destination still allows a dry run and fails before any move.
    let listed_dest = search::lookup_folder(session, dest)?;
    let safe_dest = match &listed_dest {
        Some(name) => sanitize_folder_name(name),
        None => sanitize_terminal_field(dest),
    };
    let sp = spinner("Searching...");
    let mut messages = search::search_excluding(
        session,
        criteria,
        Some(listed_dest.as_deref().unwrap_or(dest)),
    )?;
    sp.finish_and_clear();
    if let Some(account) = account_name {
        for msg in &mut messages {
            msg.account = Some(account.to_string());
        }
    }

    if messages.is_empty() {
        println!("No messages match the criteria.");
        return Ok(());
    }

    display_messages(&messages);

    if dry_run {
        println!(
            "Dry run: {} message(s) would be moved to {safe_dest}.",
            messages.len()
        );
        return Ok(());
    }

    session.ensure_safe_move_supported()?;
    let dest = listed_dest.ok_or_else(|| search::missing_folder(dest))?;
    // Validate every row's mailbox identity before any mutation.
    let groups = search::group_message_uids(&messages, &criteria.folder)?;

    if !yes {
        let confirm = inquire::Confirm::new(&format!(
            "Move {} message(s) to {safe_dest}?",
            messages.len()
        ))
        .with_default(false)
        .prompt()
        .context("Prompt failed")?;

        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let sp = spinner(&format!("Moving to {safe_dest}..."));

    let mut total = 0usize;
    let mut move_groups = || -> Result<()> {
        for (folder, (uid_validity, uids)) in &groups {
            let failed = |total: usize| {
                format!(
                    "Failed to move messages from '{}' to '{safe_dest}' ({total} already moved)",
                    sanitize_folder_name(folder)
                )
            };
            search::select_verified(session, folder, *uid_validity)
                .with_context(|| failed(total))?;
            for chunk in &search::build_uid_set(uids) {
                let present =
                    search::existing_uids(session, chunk).with_context(|| failed(total))?;
                for set in &search::build_uid_set(&present) {
                    session
                        .uid_move_or_fallback(set, &dest)
                        .with_context(|| failed(total))?;
                    total += search::uid_set_len(set);
                }
            }
        }
        Ok(())
    };
    let result = move_groups();
    sp.finish_and_clear();
    result?;

    println!("Moved {total} message(s) to {safe_dest}.");
    report_vanished(messages.len(), total);
    Ok(())
}

/// Report searched messages that another client removed before the action,
/// so receipts count only messages the server still had.
pub fn report_vanished(searched: usize, acted: usize) {
    let vanished = searched.saturating_sub(acted);
    if vanished > 0 {
        println!("{vanished} message(s) no longer existed and were skipped.");
    }
}

pub fn delete(
    session: &mut ImapSession,
    criteria: &mut SearchCriteria,
    trash_folder: &str,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    delete_with_account(session, criteria, trash_folder, yes, dry_run, None)
}

pub fn delete_with_account(
    session: &mut ImapSession,
    criteria: &mut SearchCriteria,
    trash_folder: &str,
    yes: bool,
    dry_run: bool,
    account_name: Option<&str>,
) -> Result<()> {
    search_and_move_with_account(session, criteria, trash_folder, yes, dry_run, account_name)
}
