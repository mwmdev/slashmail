use slashmail::{config, connection, delete, display, export, search};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, Table};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use zeroize::Zeroize;

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

fn quota_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\*\s+QUOTA\s+.*?\(([^)]+)\)").unwrap())
}

fn quota_resource_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\w+)\s+(\d+)\s+(\d+)").unwrap())
}

fn status_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\*\s+STATUS\s+.*?\(([^)]*)\)").unwrap())
}

#[derive(Parser)]
#[command(
    name = "slashmail",
    about = "IMAP CLI for searching, managing, and inspecting email"
)]
struct Cli {
    /// IMAP host [default: 127.0.0.1]
    #[arg(long, global = true)]
    host: Option<String>,

    /// IMAP port [default: 1143 plain, 993 TLS]
    #[arg(long, global = true)]
    port: Option<u16>,

    /// Use TLS (required for remote IMAP servers)
    #[arg(long, global = true)]
    tls: bool,

    /// IMAP username
    #[arg(short, long, env = "SLASHMAIL_USER", global = true)]
    user: Option<String>,

    /// Path to config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// IMAP password (or SLASHMAIL_PASS env; prompts if missing)
    #[arg(skip)]
    _pass_placeholder: (),

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Search messages by criteria
    Search(SearchArgs),
    /// Search + delete matching messages (move to Trash)
    Delete(DeleteArgs),
    /// Search + move matching messages to a folder
    Move(MoveArgs),
    /// Search + export matching messages as .eml files
    Export(ExportArgs),
    /// Search + set/unset flags on matching messages
    Mark(MarkArgs),
    /// Count matching messages (no FETCH)
    Count(CountArgs),
    /// Show mailbox quota usage
    Quota,
    /// Show per-folder message statistics
    Status,
    /// Generate shell completions
    Completions {
        /// Shell to generate for (bash, zsh, fish, powershell, elvish)
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate man page
    #[command(hide = true)]
    Manpage,
}

#[derive(Parser)]
struct FilterArgs {
    /// Folder to search [default: INBOX]
    #[arg(short, long)]
    folder: Option<String>,

    /// Search across all folders (excludes Trash, Spam)
    #[arg(long)]
    all_folders: bool,

    /// Subject contains
    #[arg(long)]
    subject: Option<String>,

    /// From address contains
    #[arg(long)]
    from: Option<String>,

    /// To address contains
    #[arg(long)]
    to: Option<String>,

    /// CC address contains
    #[arg(long)]
    cc: Option<String>,

    /// Messages since date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    #[arg(long)]
    since: Option<String>,

    /// Messages before date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    #[arg(long)]
    before: Option<String>,

    /// Messages larger than N bytes (supports K/M suffix)
    #[arg(long)]
    larger: Option<String>,
}

#[derive(Parser)]
struct SearchArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Limit number of results
    #[arg(short = 'n', long)]
    limit: Option<usize>,
}

#[derive(Parser)]
struct DeleteArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Destination trash folder [default: Trash]
    #[arg(long)]
    trash_folder: Option<String>,

    /// Limit number of messages to act on
    #[arg(short = 'n', long)]
    limit: Option<usize>,

    /// Skip confirmation (batch mode)
    #[arg(long)]
    yes: bool,

    /// Show what would be deleted without acting
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser)]
struct MoveArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Destination folder
    #[arg(long)]
    to: String,

    /// Limit number of messages to act on
    #[arg(short = 'n', long)]
    limit: Option<usize>,

    /// Skip confirmation
    #[arg(long)]
    yes: bool,

    /// Show what would be moved without acting
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser)]
struct ExportArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Limit number of results
    #[arg(short = 'n', long)]
    limit: Option<usize>,

    /// Output directory for .eml files (default: current directory)
    #[arg(short, long)]
    output_dir: Option<PathBuf>,

    /// Skip confirmation
    #[arg(long)]
    yes: bool,

    /// Overwrite existing .eml files
    #[arg(long)]
    force: bool,
}

#[derive(Parser)]
struct MarkArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Mark as read (\Seen)
    #[arg(long)]
    read: bool,

    /// Mark as unread (remove \Seen)
    #[arg(long)]
    unread: bool,

    /// Set \Flagged
    #[arg(long)]
    flagged: bool,

    /// Remove \Flagged
    #[arg(long)]
    unflagged: bool,

    /// Limit number of messages to act on
    #[arg(short = 'n', long)]
    limit: Option<usize>,

    /// Skip confirmation
    #[arg(long)]
    yes: bool,

    /// Show what would be changed without acting
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser)]
struct CountArgs {
    #[command(flatten)]
    filter: FilterArgs,
}

impl FilterArgs {
    fn to_criteria(&self, limit: Option<usize>, default_folder: &str) -> search::SearchCriteria {
        search::SearchCriteria {
            folder: self
                .folder
                .clone()
                .unwrap_or_else(|| default_folder.to_string()),
            all_folders: self.all_folders,
            subject: self.subject.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            cc: self.cc.clone(),
            since: self.since.clone(),
            before: self.before.clone(),
            larger: self.larger.clone(),
            limit,
        }
    }
}

fn get_password() -> Result<String> {
    if let Ok(p) = std::env::var("SLASHMAIL_PASS") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    inquire::Password::new("IMAP password:")
        .without_confirmation()
        .prompt()
        .context("Password prompt failed")
}

fn cmd_quota(session: &mut connection::ImapSession) -> Result<()> {
    if !session.has_capability("QUOTA") {
        bail!("Server does not support QUOTA extension (RFC 2087)");
    }

    let sp = spinner("Fetching quota...");
    let response = session
        .run_command_and_read_response("GETQUOTAROOT INBOX")
        .context("GETQUOTAROOT failed")?;
    sp.finish_and_clear();

    let text = String::from_utf8_lossy(&response);

    // Parse: * QUOTA "root" (STORAGE used limit) (MESSAGE used limit) ...
    let mut rows: Vec<(String, u64, u64)> = Vec::new();
    for cap in quota_regex().captures_iter(&text) {
        let inner = &cap[1];
        if let Some(m) = quota_resource_regex().captures(inner) {
            let name = m[1].to_string();
            let used: u64 = m[2].parse().unwrap_or(0);
            let limit: u64 = m[3].parse().unwrap_or(0);
            rows.push((name, used, limit));
        }
    }

    if rows.is_empty() {
        println!("No quota information available.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Resource", "Used", "Limit", "Usage"]);

    for (name, used, limit) in &rows {
        let (used_str, limit_str) = if name.eq_ignore_ascii_case("STORAGE") {
            // STORAGE values are in KB
            (
                display::format_size(used * 1024),
                display::format_size(limit * 1024),
            )
        } else {
            (used.to_string(), limit.to_string())
        };

        let pct = if *limit > 0 {
            *used as f64 / *limit as f64 * 100.0
        } else {
            0.0
        };
        let pct_str = format!("{pct:.1}%");

        let mut row = vec![Cell::new(name), Cell::new(&used_str), Cell::new(&limit_str)];
        let pct_cell = if pct >= 90.0 {
            Cell::new(&pct_str).fg(Color::Red)
        } else if pct >= 75.0 {
            Cell::new(&pct_str).fg(Color::Yellow)
        } else {
            Cell::new(&pct_str)
        };
        row.push(pct_cell);
        table.add_row(row);
    }

    println!("{table}");
    Ok(())
}

fn cmd_status(session: &mut connection::ImapSession) -> Result<()> {
    let sp = spinner("Fetching folder status...");
    let folders = session
        .list(Some(""), Some("*"))
        .context("Failed to list folders")?;
    let folder_names: Vec<String> = folders.iter().map(|f| f.name().to_string()).collect();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Folder", "Messages", "Unseen", "Recent"]);

    let mut total_messages: u32 = 0;
    let mut total_unseen: u32 = 0;
    let mut total_recent: u32 = 0;

    for name in &folder_names {
        // Folder names are server-controlled, so always quote via imap_quote()
        // which strips control chars and escapes IMAP-special characters.
        let quoted = search::imap_quote(name);
        let cmd = format!("STATUS {quoted} (MESSAGES UNSEEN RECENT)");
        let response = match session.run_command_and_read_response(&cmd) {
            Ok(r) => r,
            Err(_) => {
                table.add_row(vec![name.as_str(), "?", "?", "?"]);
                continue;
            }
        };

        let text = String::from_utf8_lossy(&response);
        let mut messages: u32 = 0;
        let mut unseen: u32 = 0;
        let mut recent: u32 = 0;

        if let Some(cap) = status_regex().captures(&text) {
            let attrs = &cap[1];
            // Parse key-value pairs: MESSAGES 142 UNSEEN 12 RECENT 3
            let tokens: Vec<&str> = attrs.split_whitespace().collect();
            for pair in tokens.chunks(2) {
                if pair.len() == 2 {
                    let val: u32 = pair[1].parse().unwrap_or(0);
                    match pair[0].to_uppercase().as_str() {
                        "MESSAGES" => messages = val,
                        "UNSEEN" => unseen = val,
                        "RECENT" => recent = val,
                        _ => {}
                    }
                }
            }
        }

        total_messages += messages;
        total_unseen += unseen;
        total_recent += recent;

        table.add_row(vec![
            name.as_str(),
            &messages.to_string(),
            &unseen.to_string(),
            &recent.to_string(),
        ]);
    }

    sp.finish_and_clear();

    // Total row
    table.add_row(vec![
        Cell::new("Total").fg(Color::Cyan),
        Cell::new(total_messages).fg(Color::Cyan),
        Cell::new(total_unseen).fg(Color::Cyan),
        Cell::new(total_recent).fg(Color::Cyan),
    ]);

    println!("{table}");
    Ok(())
}

fn cmd_export(
    session: &mut connection::ImapSession,
    args: &ExportArgs,
    default_folder: &str,
) -> Result<()> {
    let criteria = args.filter.to_criteria(args.limit, default_folder);
    let sp = spinner("Searching...");
    let messages = search::search(session, &criteria)?;
    sp.finish_and_clear();

    if messages.is_empty() {
        println!("No messages found.");
        return Ok(());
    }

    display::display_messages(&messages);

    let out_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    if !args.yes {
        let confirm = inquire::Confirm::new(&format!(
            "Export {} message(s) to {}?",
            messages.len(),
            out_dir.display()
        ))
        .with_default(false)
        .prompt()
        .context("Prompt failed")?;

        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let sp = spinner("Exporting...");
    let (exported, skipped) =
        export::export_messages(session, &messages, &criteria.folder, &out_dir, args.force)?;
    sp.finish_and_clear();

    print!("Exported {exported} message(s) to {}", out_dir.display());
    if skipped > 0 {
        print!(" ({skipped} skipped, already exist)");
    }
    println!();
    Ok(())
}

fn validate_mark_flags(read: bool, unread: bool, flagged: bool, unflagged: bool) -> Result<()> {
    if !read && !unread && !flagged && !unflagged {
        bail!("Specify at least one flag: --read, --unread, --flagged, --unflagged");
    }
    if read && unread {
        bail!("Cannot use --read and --unread together");
    }
    if flagged && unflagged {
        bail!("Cannot use --flagged and --unflagged together");
    }
    Ok(())
}

fn mark_store_ops(read: bool, unread: bool, flagged: bool, unflagged: bool) -> Vec<String> {
    let mut ops = Vec::new();
    if read {
        ops.push("+FLAGS (\\Seen)".to_string());
    }
    if unread {
        ops.push("-FLAGS (\\Seen)".to_string());
    }
    if flagged {
        ops.push("+FLAGS (\\Flagged)".to_string());
    }
    if unflagged {
        ops.push("-FLAGS (\\Flagged)".to_string());
    }
    ops
}

fn mark_action_desc(read: bool, unread: bool, flagged: bool, unflagged: bool) -> String {
    let mut actions = Vec::new();
    if read {
        actions.push("mark read");
    }
    if unread {
        actions.push("mark unread");
    }
    if flagged {
        actions.push("flag");
    }
    if unflagged {
        actions.push("unflag");
    }
    actions.join(" + ")
}

fn cmd_mark(
    session: &mut connection::ImapSession,
    args: &MarkArgs,
    default_folder: &str,
) -> Result<()> {
    validate_mark_flags(args.read, args.unread, args.flagged, args.unflagged)?;

    let criteria = args.filter.to_criteria(args.limit, default_folder);
    let sp = spinner("Searching...");
    let messages = search::search(session, &criteria)?;
    sp.finish_and_clear();

    if messages.is_empty() {
        println!("No messages match the criteria.");
        return Ok(());
    }

    display::display_messages(&messages);

    let action_desc = mark_action_desc(args.read, args.unread, args.flagged, args.unflagged);

    if args.dry_run {
        println!(
            "Dry run: would {action_desc} {} message(s).",
            messages.len()
        );
        return Ok(());
    }

    if !args.yes {
        let confirm =
            inquire::Confirm::new(&format!("{action_desc} {} message(s)?", messages.len()))
                .with_default(false)
                .prompt()
                .context("Prompt failed")?;

        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let store_ops = mark_store_ops(args.read, args.unread, args.flagged, args.unflagged);

    let sp = spinner("Updating flags...");

    // Group by folder
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
            for op in &store_ops {
                session
                    .uid_store(chunk, op)
                    .with_context(|| format!("Failed to store flags in '{folder}'"))?;
            }
        }

        total += uids.len();
    }

    sp.finish_and_clear();
    println!("Updated {total} message(s).");
    Ok(())
}

fn cmd_count(
    session: &mut connection::ImapSession,
    args: &CountArgs,
    default_folder: &str,
) -> Result<()> {
    let criteria = args.filter.to_criteria(None, default_folder);
    let query = search::build_query(&criteria)?;

    let sp = spinner("Counting...");

    if criteria.all_folders {
        let folders = session
            .list(Some(""), Some("*"))
            .context("Failed to list folders")?;
        let folder_names: Vec<String> = folders
            .iter()
            .map(|f| f.name().to_string())
            .filter(|n| !search::folders_to_skip(n))
            .collect();

        let mut grand_total = 0usize;
        let mut results: Vec<(String, usize)> = Vec::new();

        for folder in &folder_names {
            match session.select(folder) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Warning: skipping folder '{folder}': {e}");
                    continue;
                }
            }
            match session.uid_search(&query) {
                Ok(uids) => {
                    let count = uids.len();
                    if count > 0 {
                        results.push((folder.clone(), count));
                        grand_total += count;
                    }
                }
                Err(e) => {
                    eprintln!("Warning: search failed in '{folder}': {e}");
                }
            }
        }

        sp.finish_and_clear();

        if results.is_empty() {
            println!("0 message(s) match.");
        } else {
            for (folder, count) in &results {
                println!("{count} message(s) in {folder}");
            }
            if results.len() > 1 {
                println!("{grand_total} message(s) total");
            }
        }
    } else {
        session
            .select(&criteria.folder)
            .with_context(|| format!("Failed to select '{}'", criteria.folder))?;

        let uids = session.uid_search(&query).context("IMAP SEARCH failed")?;
        sp.finish_and_clear();
        println!("{} message(s) in {}", uids.len(), criteria.folder);
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle commands that don't need an IMAP connection
    match &cli.command {
        Commands::Completions { shell } => {
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "slashmail",
                &mut std::io::stdout(),
            );
            return Ok(());
        }
        Commands::Manpage => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            return Ok(());
        }
        _ => {}
    }

    // Load config: explicit --config path > default location > empty
    let cfg = config::Config::load(cli.config.as_deref())?;

    // Resolve values: CLI/env > config > built-in default
    let tls = cli.tls || cfg.tls.unwrap_or(false);
    let host = cli
        .host
        .or(cfg.host)
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = cli
        .port
        .or(cfg.port)
        .unwrap_or(if tls { 993 } else { 1143 });
    let user = cli.user.or(cfg.user).ok_or_else(|| {
        anyhow::anyhow!("IMAP username required (use -u/--user or SLASHMAIL_USER env)")
    })?;
    let default_folder = cfg.default_folder.unwrap_or_else(|| "INBOX".to_string());
    let default_trash = cfg.trash_folder.unwrap_or_else(|| "Trash".to_string());

    let mut pass = get_password()?;

    let sp = spinner("Connecting...");
    let session_result = connection::connect(&host, port, tls, &user, &pass);
    sp.finish_and_clear();

    // Clear password from memory on both success and error paths.
    pass.zeroize();

    let mut session = session_result?;

    let result = match &cli.command {
        Commands::Search(args) => {
            let criteria = args.filter.to_criteria(args.limit, &default_folder);
            let sp = spinner("Searching...");
            let messages = search::search(&mut session, &criteria)?;
            sp.finish_and_clear();
            display::display_messages(&messages);
            Ok(())
        }
        Commands::Delete(args) => {
            let criteria = args.filter.to_criteria(args.limit, &default_folder);
            let trash = args.trash_folder.as_deref().unwrap_or(&default_trash);
            delete::delete(&mut session, &criteria, trash, args.yes, args.dry_run)
        }
        Commands::Move(args) => {
            let criteria = args.filter.to_criteria(args.limit, &default_folder);
            delete::search_and_move(&mut session, &criteria, &args.to, args.yes, args.dry_run)
        }
        Commands::Export(args) => cmd_export(&mut session, args, &default_folder),
        Commands::Mark(args) => cmd_mark(&mut session, args, &default_folder),
        Commands::Count(args) => cmd_count(&mut session, args, &default_folder),
        Commands::Quota => cmd_quota(&mut session),
        Commands::Status => cmd_status(&mut session),
        Commands::Completions { .. } | Commands::Manpage => unreachable!(),
    };

    let _ = session.logout();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_mark_flags_no_flags() {
        assert!(validate_mark_flags(false, false, false, false).is_err());
    }

    #[test]
    fn validate_mark_flags_read_and_unread() {
        assert!(validate_mark_flags(true, true, false, false).is_err());
    }

    #[test]
    fn validate_mark_flags_flagged_and_unflagged() {
        assert!(validate_mark_flags(false, false, true, true).is_err());
    }

    #[test]
    fn validate_mark_flags_single_flag() {
        assert!(validate_mark_flags(true, false, false, false).is_ok());
        assert!(validate_mark_flags(false, true, false, false).is_ok());
        assert!(validate_mark_flags(false, false, true, false).is_ok());
        assert!(validate_mark_flags(false, false, false, true).is_ok());
    }

    #[test]
    fn validate_mark_flags_valid_combo() {
        assert!(validate_mark_flags(true, false, true, false).is_ok());
        assert!(validate_mark_flags(false, true, false, true).is_ok());
        assert!(validate_mark_flags(true, false, false, true).is_ok());
    }

    #[test]
    fn mark_store_ops_read() {
        assert_eq!(
            mark_store_ops(true, false, false, false),
            vec!["+FLAGS (\\Seen)"]
        );
    }

    #[test]
    fn mark_store_ops_unread() {
        assert_eq!(
            mark_store_ops(false, true, false, false),
            vec!["-FLAGS (\\Seen)"]
        );
    }

    #[test]
    fn mark_store_ops_flagged() {
        assert_eq!(
            mark_store_ops(false, false, true, false),
            vec!["+FLAGS (\\Flagged)"]
        );
    }

    #[test]
    fn mark_store_ops_unflagged() {
        assert_eq!(
            mark_store_ops(false, false, false, true),
            vec!["-FLAGS (\\Flagged)"]
        );
    }

    #[test]
    fn mark_store_ops_combo() {
        let ops = mark_store_ops(true, false, true, false);
        assert_eq!(ops, vec!["+FLAGS (\\Seen)", "+FLAGS (\\Flagged)"]);
    }

    #[test]
    fn mark_action_desc_single() {
        assert_eq!(mark_action_desc(true, false, false, false), "mark read");
        assert_eq!(mark_action_desc(false, true, false, false), "mark unread");
        assert_eq!(mark_action_desc(false, false, true, false), "flag");
        assert_eq!(mark_action_desc(false, false, false, true), "unflag");
    }

    #[test]
    fn mark_action_desc_combo() {
        assert_eq!(
            mark_action_desc(true, false, true, false),
            "mark read + flag"
        );
    }
}
