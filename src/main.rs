use slashmail::{config, connection, delete, display, draft, export, read, search};

use anyhow::{bail, Context, Result};
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, Table};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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

    /// Named account from config to use
    #[arg(long, global = true, conflicts_with = "all_accounts")]
    account: Option<String>,

    /// Query all configured accounts (read-only commands only)
    #[arg(long, global = true, conflicts_with = "account")]
    all_accounts: bool,

    /// IMAP password (or SLASHMAIL_PASS env; prompts if missing)
    #[arg(skip)]
    _pass_placeholder: (),

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a new unsent email draft
    Draft(DraftArgs),
    /// Save an unsent reply draft for one message UID
    Reply(ReplyArgs),
    /// Search messages by criteria
    Search(SearchArgs),
    /// Display the content of matching messages
    Read(ReadArgs),
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
struct DraftArgs {
    /// Recipient mailbox (repeat once per recipient)
    #[arg(long, required = true)]
    to: Vec<String>,

    /// Cc mailbox (repeat once per recipient)
    #[arg(long)]
    cc: Vec<String>,

    /// Bcc mailbox (repeat once per recipient)
    #[arg(long)]
    bcc: Vec<String>,

    /// Draft subject
    #[arg(long, default_value = "")]
    subject: String,

    /// Treat stdin as an HTML body instead of plain text
    #[arg(long)]
    html: bool,

    /// Local file to attach (repeat once per file)
    #[arg(long, value_name = "PATH")]
    attach: Vec<PathBuf>,

    /// Destination Drafts mailbox
    #[arg(long)]
    drafts_folder: Option<String>,
}

#[derive(Parser)]
struct ReplyArgs {
    /// UID of the source message
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    uid: u32,

    /// Folder containing the source message
    #[arg(short, long)]
    folder: Option<String>,

    /// Treat stdin as an HTML body instead of plain text
    #[arg(long)]
    html: bool,

    /// Omit the quoted original message
    #[arg(long)]
    no_quote: bool,

    /// Local file to attach (repeat once per file)
    #[arg(long, value_name = "PATH")]
    attach: Vec<PathBuf>,

    /// Destination Drafts mailbox
    #[arg(long)]
    drafts_folder: Option<String>,
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

    /// Message body contains
    #[arg(long)]
    body: Option<String>,

    /// Headers or body contains
    #[arg(long)]
    text: Option<String>,

    /// Only read messages
    #[arg(long, conflicts_with = "unseen")]
    seen: bool,

    /// Only unread messages
    #[arg(long, conflicts_with = "seen")]
    unseen: bool,

    /// Messages since date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    #[arg(long)]
    since: Option<String>,

    /// Messages before date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    #[arg(long)]
    before: Option<String>,

    /// Messages larger than N bytes (supports K/M suffix)
    #[arg(long)]
    larger: Option<String>,

    /// Messages smaller than N bytes (supports K/M suffix)
    #[arg(long)]
    smaller: Option<String>,

    /// Only flagged/starred messages
    #[arg(long, conflicts_with = "unflagged")]
    flagged: bool,

    /// Only unflagged messages
    #[arg(long, conflicts_with = "flagged")]
    unflagged: bool,

    /// Only replied-to messages
    #[arg(long)]
    answered: bool,

    /// Only draft messages
    #[arg(long)]
    draft: bool,
}

#[derive(Parser)]
struct SearchArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Limit number of results
    #[arg(short = 'n', long)]
    limit: Option<usize>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct ReadArgs {
    #[command(flatten)]
    filter: FilterArgs,

    /// Limit number of messages to display [default: 1]
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

    /// Output as JSON
    #[arg(long)]
    json: bool,
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
            body: self.body.clone(),
            text: self.text.clone(),
            seen: self.seen,
            unseen: self.unseen,
            since: self.since.clone(),
            before: self.before.clone(),
            larger: self.larger.clone(),
            smaller: self.smaller.clone(),
            flagged: self.flagged,
            unflagged: self.unflagged,
            answered: self.answered,
            draft: self.draft,
            limit,
        }
    }
}

fn get_password_for_account(account: &config::ResolvedAccount) -> Result<String> {
    if let Some(env_name) = &account.pass_env {
        if let Ok(p) = std::env::var(env_name) {
            if !p.is_empty() {
                return Ok(p);
            }
        }
    } else if account.name.is_none() {
        if let Ok(p) = std::env::var("SLASHMAIL_PASS") {
            if !p.is_empty() {
                return Ok(p);
            }
        }
    }

    let prompt = account
        .name
        .as_ref()
        .map(|name| format!("IMAP password for {name}:"))
        .unwrap_or_else(|| "IMAP password:".to_string());
    inquire::Password::new(&prompt)
        .without_confirmation()
        .prompt()
        .context("Password prompt failed")
}

struct DraftCredential(String);

impl DraftCredential {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for DraftCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn draft_credential_with<F>(
    account: &config::ResolvedAccount,
    mut environment: F,
) -> Result<DraftCredential>
where
    F: FnMut(&str) -> Option<String>,
{
    let variable = match account.pass_env.as_deref() {
        Some(variable) => variable,
        None if account.name.is_none() => "SLASHMAIL_PASS",
        None => {
            bail!(
                "Account '{}' must configure pass_env for draft and reply commands",
                account.label()
            )
        }
    };
    let password = environment(variable)
        .filter(|password| !password.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Required password environment variable '{variable}' is missing or empty"
            )
        })?;
    Ok(DraftCredential(password))
}

fn read_draft_body_with<R, F>(
    account: &config::ResolvedAccount,
    environment: F,
    reader: R,
) -> Result<(DraftCredential, String)>
where
    R: Read,
    F: FnMut(&str) -> Option<String>,
{
    let (credential, attachments, body) = prepare_draft_with(account, &[], environment, reader)?;
    debug_assert!(attachments.is_empty());
    Ok((credential, body))
}

fn prepare_draft_with<R, F>(
    account: &config::ResolvedAccount,
    attachment_paths: &[PathBuf],
    environment: F,
    mut reader: R,
) -> Result<(DraftCredential, Vec<draft::DraftAttachment>, String)>
where
    R: Read,
    F: FnMut(&str) -> Option<String>,
{
    ensure_secure_draft_transport(account)?;
    let credential = draft_credential_with(account, environment)?;
    let attachments = load_attachments(attachment_paths)?;
    let mut body = String::new();
    reader
        .read_to_string(&mut body)
        .context("Failed to read the draft body from stdin")?;
    Ok((credential, attachments, body))
}

fn load_attachments(paths: &[PathBuf]) -> Result<Vec<draft::DraftAttachment>> {
    paths.iter().map(|path| load_attachment(path)).collect()
}

fn load_attachment(path: &Path) -> Result<draft::DraftAttachment> {
    let safe_path = safe_attachment_path(path);
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("Attachment path {safe_path} has no valid Unicode basename")
        })?
        .to_string();
    if filename.chars().any(disallowed_attachment_name_character) {
        bail!("Attachment path {safe_path} has a disallowed basename");
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .with_context(|| format!("Failed to open attachment {safe_path}"))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("Failed to inspect attachment {safe_path}"))?;
    if !metadata.file_type().is_file() {
        bail!("Attachment path {safe_path} is not a regular file");
    }
    let bytes = read_attachment_bytes(&mut file, &safe_path)?;
    let content_type = mime_guess::from_ext(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or(""),
    )
    .first_or_octet_stream()
    .into();

    Ok(draft::DraftAttachment {
        filename,
        content_type,
        bytes,
    })
}

fn read_attachment_bytes(file: &mut File, safe_path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to read attachment {safe_path}"));
            }
        };
        if read == 0 {
            return Ok(bytes);
        }
        bytes
            .try_reserve_exact(read)
            .with_context(|| format!("Failed to allocate memory for attachment {safe_path}"))?;
        bytes.extend_from_slice(&buffer[..read]);
    }
}

fn disallowed_attachment_name_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn safe_attachment_path(path: &Path) -> String {
    let raw = path.as_os_str().to_string_lossy();
    let mut escaped = String::from("\"");
    for character in raw.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            character if disallowed_attachment_name_character(character) => {
                escaped.push_str(&format!("\\u{{{:x}}}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn ensure_secure_draft_transport(account: &config::ResolvedAccount) -> Result<()> {
    if account.tls || connection::is_loopback_host(&account.host) {
        return Ok(());
    }

    bail!("Draft and reply commands require TLS for non-loopback IMAP servers")
}

struct DraftSessionFactory<'a> {
    account: &'a config::ResolvedAccount,
    credential: DraftCredential,
}

impl DraftSessionFactory<'_> {
    fn connect(&self) -> Result<connection::ImapSession> {
        connection::connect(
            &self.account.host,
            self.account.port,
            self.account.tls,
            &self.account.user,
            self.credential.expose(),
        )
        .map_err(|_| anyhow::anyhow!("Failed to connect or authenticate the draft account"))
    }
}

fn draft_sender(account: &config::ResolvedAccount) -> Result<lettre::message::Mailbox> {
    let value = account.sender.as_deref().unwrap_or(&account.user);
    draft::parse_mailbox(value, "sender").map_err(|_| {
        if account.sender.is_some() {
            anyhow::anyhow!("The configured sender is not a valid mailbox")
        } else {
            anyhow::anyhow!(
                "The IMAP username is not a sender mailbox; configure sender for this account"
            )
        }
    })
}

fn parse_recipient_flags(values: &[String], field: &str) -> Result<Vec<lettre::message::Mailbox>> {
    values
        .iter()
        .map(|value| draft::parse_mailbox(value, field))
        .collect()
}

fn resolve_drafts_folder(
    session: &mut connection::ImapSession,
    account: &config::ResolvedAccount,
    command_override: Option<&str>,
) -> Result<String> {
    let mailboxes = draft::DraftMailboxSession::list_mailboxes(session)?;
    draft::resolve_destination(
        command_override,
        account.drafts_folder.as_deref(),
        &mailboxes,
    )
}

fn fetch_reply_source(
    session: &mut connection::ImapSession,
    folder: &str,
    uid: u32,
) -> Result<Vec<u8>> {
    session
        .examine(folder)
        .map_err(|_| anyhow::anyhow!("Failed to examine the reply source folder"))?;
    let fetches = session
        .uid_fetch(&uid.to_string(), "BODY.PEEK[]")
        .map_err(|_| anyhow::anyhow!("Failed to fetch the reply source message"))?;
    let messages = fetches
        .iter()
        .map(|fetch| draft::MessageFetch {
            uid: fetch.uid,
            body: fetch.body().map(<[u8]>::to_vec),
        })
        .collect();
    draft::require_exact_source(messages, uid)
}

fn draft_receipt(
    account: &config::ResolvedAccount,
    folder: &str,
    uid: u32,
    composed: &draft::ComposedDraft,
) -> draft::DraftReceipt {
    draft::DraftReceipt {
        account: account.label().to_string(),
        folder: folder.to_string(),
        uid,
        to: composed.to.iter().map(ToString::to_string).collect(),
        cc: composed.cc.iter().map(ToString::to_string).collect(),
        bcc: composed.bcc.iter().map(ToString::to_string).collect(),
        subject: composed.subject.clone(),
    }
}

fn report_save_outcome(
    outcome: draft::SaveOutcome,
    account: &config::ResolvedAccount,
    folder: &str,
    composed: &draft::ComposedDraft,
) -> Result<()> {
    match outcome {
        draft::SaveOutcome::Saved { uid } => {
            println!(
                "{}",
                draft::render_receipt(&draft_receipt(account, folder, uid, composed))
            );
            Ok(())
        }
        draft::SaveOutcome::SavedUidUnresolved => {
            bail!(
                "The draft was saved, but its UID could not be resolved; inspect the Drafts folder before retrying"
            )
        }
        draft::SaveOutcome::Unknown => {
            bail!(
                "The APPEND outcome is unknown after the connection was lost; inspect the Drafts folder before retrying"
            )
        }
    }
}

fn save_draft(
    factory: &DraftSessionFactory<'_>,
    session: &mut connection::ImapSession,
    folder: &str,
    composed: &draft::ComposedDraft,
) -> Result<draft::SaveOutcome> {
    draft::save_composed_draft(
        session,
        || {
            factory
                .connect()
                .map(|session| Box::new(session) as Box<dyn draft::DraftMailboxSession>)
        },
        folder,
        composed,
    )
}

fn cmd_draft(account: &config::ResolvedAccount, args: &DraftArgs) -> Result<()> {
    let sender = draft_sender(account)?;
    let to = parse_recipient_flags(&args.to, "To")?;
    let cc = parse_recipient_flags(&args.cc, "Cc")?;
    let bcc = parse_recipient_flags(&args.bcc, "Bcc")?;
    let (credential, attachments, body) = prepare_draft_with(
        account,
        &args.attach,
        |name| std::env::var(name).ok(),
        std::io::stdin(),
    )?;
    let factory = DraftSessionFactory {
        account,
        credential,
    };
    let mut session = factory.connect()?;
    let folder = resolve_drafts_folder(&mut session, account, args.drafts_folder.as_deref())?;
    let composed = draft::compose_new_draft(draft::NewDraftInput {
        sender,
        to,
        cc,
        bcc,
        subject: args.subject.clone(),
        body,
        format: if args.html {
            draft::BodyFormat::Html
        } else {
            draft::BodyFormat::Plain
        },
        attachments,
    })?;
    let outcome = save_draft(&factory, &mut session, &folder, &composed);
    let _ = session.logout();
    report_save_outcome(outcome?, account, &folder, &composed)
}

fn cmd_reply(account: &config::ResolvedAccount, args: &ReplyArgs) -> Result<()> {
    let sender = draft_sender(account)?;
    let (credential, attachments, body) = prepare_draft_with(
        account,
        &args.attach,
        |name| std::env::var(name).ok(),
        std::io::stdin(),
    )?;
    let factory = DraftSessionFactory {
        account,
        credential,
    };
    let mut session = factory.connect()?;
    let folder = resolve_drafts_folder(&mut session, account, args.drafts_folder.as_deref())?;
    let source_folder = args.folder.as_deref().unwrap_or(&account.default_folder);
    let source = fetch_reply_source(&mut session, source_folder, args.uid)?;
    let composed = draft::compose_reply_draft(draft::ReplyDraftInput {
        sender,
        source: &source,
        body,
        format: if args.html {
            draft::BodyFormat::Html
        } else {
            draft::BodyFormat::Plain
        },
        quote_original: !args.no_quote,
        attachments,
    })?;
    let outcome = save_draft(&factory, &mut session, &folder, &composed);
    let _ = session.logout();
    report_save_outcome(outcome?, account, &folder, &composed)
}

fn with_account_session<T, F>(account: &config::ResolvedAccount, f: F) -> Result<T>
where
    F: FnOnce(&mut connection::ImapSession) -> Result<T>,
{
    let mut pass = get_password_for_account(account)?;

    let sp = spinner(&format!("Connecting to {}...", account.label()));
    let session_result = connection::connect(
        &account.host,
        account.port,
        account.tls,
        &account.user,
        &pass,
    )
    .with_context(|| format!("Account '{}'", account.label()));
    sp.finish_and_clear();

    // Clear password from memory on both success and error paths.
    pass.zeroize();

    let mut session = session_result?;
    let result = f(&mut session);
    let _ = session.logout();
    result
}

fn tag_messages(messages: &mut [display::MessageRow], account: &config::ResolvedAccount) {
    if let Some(name) = &account.name {
        for msg in messages {
            msg.account = Some(name.clone());
        }
    }
}

fn sort_and_limit_messages(messages: &mut Vec<display::MessageRow>, limit: Option<usize>) {
    messages.sort_by_key(|message| std::cmp::Reverse(message.timestamp));
    if let Some(n) = limit {
        messages.truncate(n);
    }
}

fn search_accounts(
    accounts: &[config::ResolvedAccount],
    filter: &FilterArgs,
    limit: Option<usize>,
) -> Result<Vec<display::MessageRow>> {
    let mut all_messages = Vec::new();

    for account in accounts {
        let mut messages = with_account_session(account, |session| {
            let sp = spinner(&format!("Searching {}...", account.label()));
            let criteria = filter.to_criteria(limit, &account.default_folder);
            let result = search::search(session, &criteria);
            sp.finish_and_clear();
            result
        })?;
        tag_messages(&mut messages, account);
        all_messages.extend(messages);
    }

    if accounts.len() > 1 {
        sort_and_limit_messages(&mut all_messages, limit);
    }

    Ok(all_messages)
}

fn command_supports_all_accounts(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Search(_)
            | Commands::Read(_)
            | Commands::Count(_)
            | Commands::Quota
            | Commands::Status
    )
}

fn reject_all_accounts_if_unsupported(cli: &Cli) -> Result<()> {
    if cli.all_accounts && !command_supports_all_accounts(&cli.command) {
        bail!(
            "--all-accounts is only supported for search, read, count, status, and quota; use --account <NAME> for this command"
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct QuotaRow {
    account: Option<String>,
    resource: String,
    used: u64,
    limit: u64,
}

fn fetch_quota_rows(
    session: &mut connection::ImapSession,
    account: &config::ResolvedAccount,
) -> Result<Vec<QuotaRow>> {
    if !session.has_capability("QUOTA") {
        bail!(
            "Account '{}': server does not support QUOTA extension (RFC 2087)",
            account.label()
        );
    }

    let response = session
        .run_command_and_read_response("GETQUOTAROOT INBOX")
        .context("GETQUOTAROOT failed")?;

    let text = String::from_utf8_lossy(&response);

    // Parse: * QUOTA "root" (STORAGE used limit) (MESSAGE used limit) ...
    let mut rows = Vec::new();
    for cap in quota_regex().captures_iter(&text) {
        let inner = &cap[1];
        if let Some(m) = quota_resource_regex().captures(inner) {
            let used: u64 = m[2].parse().unwrap_or(0);
            let limit: u64 = m[3].parse().unwrap_or(0);
            rows.push(QuotaRow {
                account: account.name.clone(),
                resource: m[1].to_string(),
                used,
                limit,
            });
        }
    }

    Ok(rows)
}

fn display_quota_rows(rows: &[QuotaRow], include_account: bool) {
    if rows.is_empty() {
        println!("No quota information available.");
        return;
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    let mut header = vec!["Resource", "Used", "Limit", "Usage"];
    if include_account {
        header.insert(0, "Account");
    }
    table.set_header(header);

    for row in rows {
        let (used_str, limit_str) = if row.resource.eq_ignore_ascii_case("STORAGE") {
            // STORAGE values are in KB
            (
                display::format_size(row.used * 1024),
                display::format_size(row.limit * 1024),
            )
        } else {
            (row.used.to_string(), row.limit.to_string())
        };

        let pct = if row.limit > 0 {
            row.used as f64 / row.limit as f64 * 100.0
        } else {
            0.0
        };
        let pct_str = format!("{pct:.1}%");

        let mut cells = Vec::new();
        if include_account {
            cells.push(Cell::new(row.account.as_deref().unwrap_or("")));
        }
        cells.push(Cell::new(&row.resource));
        cells.push(Cell::new(&used_str));
        cells.push(Cell::new(&limit_str));
        let pct_cell = if pct >= 90.0 {
            Cell::new(&pct_str).fg(Color::Red)
        } else if pct >= 75.0 {
            Cell::new(&pct_str).fg(Color::Yellow)
        } else {
            Cell::new(&pct_str)
        };
        cells.push(pct_cell);
        table.add_row(cells);
    }

    println!("{table}");
}

fn cmd_quota_accounts(accounts: &[config::ResolvedAccount]) -> Result<()> {
    let include_account =
        accounts.len() > 1 || accounts.iter().any(|account| account.name.is_some());
    let mut rows = Vec::new();
    for account in accounts {
        let mut account_rows = with_account_session(account, |session| {
            let sp = spinner(&format!("Fetching quota for {}...", account.label()));
            let result = fetch_quota_rows(session, account);
            sp.finish_and_clear();
            result
        })?;
        rows.append(&mut account_rows);
    }
    display_quota_rows(&rows, include_account);
    Ok(())
}

#[derive(Debug, Clone)]
struct StatusRow {
    account: Option<String>,
    folder: String,
    messages: Option<u32>,
    unseen: Option<u32>,
    recent: Option<u32>,
}

fn fetch_status_rows(
    session: &mut connection::ImapSession,
    account: &config::ResolvedAccount,
) -> Result<Vec<StatusRow>> {
    let folders = session
        .list(Some(""), Some("*"))
        .context("Failed to list folders")?;
    let folder_names: Vec<String> = folders.iter().map(|f| f.name().to_string()).collect();

    let mut rows = Vec::new();

    for name in &folder_names {
        // Folder names are server-controlled, so always quote via imap_quote()
        // which strips control chars and escapes IMAP-special characters.
        let quoted = search::imap_quote(name);
        let cmd = format!("STATUS {quoted} (MESSAGES UNSEEN RECENT)");
        let response = match session.run_command_and_read_response(&cmd) {
            Ok(r) => r,
            Err(_) => {
                rows.push(StatusRow {
                    account: account.name.clone(),
                    folder: name.clone(),
                    messages: None,
                    unseen: None,
                    recent: None,
                });
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

        rows.push(StatusRow {
            account: account.name.clone(),
            folder: name.clone(),
            messages: Some(messages),
            unseen: Some(unseen),
            recent: Some(recent),
        });
    }

    Ok(rows)
}

fn status_cell(value: Option<u32>) -> Cell {
    value.map(Cell::new).unwrap_or_else(|| Cell::new("?"))
}

fn display_status_rows(rows: &[StatusRow], include_account: bool) {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    let mut header = vec!["Folder", "Messages", "Unseen", "Recent"];
    if include_account {
        header.insert(0, "Account");
    }
    table.set_header(header);

    let mut total_messages: u32 = 0;
    let mut total_unseen: u32 = 0;
    let mut total_recent: u32 = 0;

    for row in rows {
        total_messages += row.messages.unwrap_or(0);
        total_unseen += row.unseen.unwrap_or(0);
        total_recent += row.recent.unwrap_or(0);

        let mut cells = Vec::new();
        if include_account {
            cells.push(Cell::new(row.account.as_deref().unwrap_or("")));
        }
        cells.push(Cell::new(&row.folder));
        cells.push(status_cell(row.messages));
        cells.push(status_cell(row.unseen));
        cells.push(status_cell(row.recent));
        table.add_row(cells);
    }

    // Total row
    let mut total_row = Vec::new();
    if include_account {
        total_row.push(Cell::new("Total").fg(Color::Cyan));
        total_row.push(Cell::new("").fg(Color::Cyan));
    } else {
        total_row.push(Cell::new("Total").fg(Color::Cyan));
    }
    total_row.push(Cell::new(total_messages).fg(Color::Cyan));
    total_row.push(Cell::new(total_unseen).fg(Color::Cyan));
    total_row.push(Cell::new(total_recent).fg(Color::Cyan));
    table.add_row(total_row);

    println!("{table}");
}

fn cmd_status_accounts(accounts: &[config::ResolvedAccount]) -> Result<()> {
    let include_account =
        accounts.len() > 1 || accounts.iter().any(|account| account.name.is_some());
    let mut rows = Vec::new();
    for account in accounts {
        let mut account_rows = with_account_session(account, |session| {
            let sp = spinner(&format!(
                "Fetching folder status for {}...",
                account.label()
            ));
            let result = fetch_status_rows(session, account);
            sp.finish_and_clear();
            result
        })?;
        rows.append(&mut account_rows);
    }
    display_status_rows(&rows, include_account);
    Ok(())
}

fn cmd_export(
    session: &mut connection::ImapSession,
    args: &ExportArgs,
    default_folder: &str,
    account_name: Option<&str>,
) -> Result<()> {
    let criteria = args.filter.to_criteria(args.limit, default_folder);
    let sp = spinner("Searching...");
    let mut messages = search::search(session, &criteria)?;
    sp.finish_and_clear();
    if let Some(account) = account_name {
        for msg in &mut messages {
            msg.account = Some(account.to_string());
        }
    }

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
    account_name: Option<&str>,
) -> Result<()> {
    validate_mark_flags(args.read, args.unread, args.flagged, args.unflagged)?;

    let criteria = args.filter.to_criteria(args.limit, default_folder);
    let sp = spinner("Searching...");
    let mut messages = search::search(session, &criteria)?;
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

#[derive(Debug, Clone)]
struct CountRow {
    account: Option<String>,
    folder: Option<String>,
    count: usize,
}

fn count_rows_for_account(
    session: &mut connection::ImapSession,
    account: &config::ResolvedAccount,
    args: &CountArgs,
) -> Result<Vec<CountRow>> {
    let default_folder = &account.default_folder;
    let criteria = args.filter.to_criteria(None, default_folder);
    let query = search::build_query(&criteria)?;

    if criteria.all_folders {
        let folders = session
            .list(Some(""), Some("*"))
            .context("Failed to list folders")?;
        let folder_names: Vec<String> = folders
            .iter()
            .map(|f| f.name().to_string())
            .filter(|n| !search::folders_to_skip(n))
            .collect();

        let mut results = Vec::new();

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
                        results.push(CountRow {
                            account: account.name.clone(),
                            folder: Some(folder.clone()),
                            count,
                        });
                    }
                }
                Err(e) => {
                    eprintln!("Warning: search failed in '{folder}': {e}");
                }
            }
        }

        Ok(results)
    } else {
        session
            .select(&criteria.folder)
            .with_context(|| format!("Failed to select '{}'", criteria.folder))?;

        let uids = session.uid_search(&query).context("IMAP SEARCH failed")?;
        Ok(vec![CountRow {
            account: account.name.clone(),
            folder: Some(criteria.folder),
            count: uids.len(),
        }])
    }
}

fn display_count_json(
    accounts: &[config::ResolvedAccount],
    rows: &[CountRow],
    all_folders: bool,
    include_account: bool,
) {
    let total: usize = rows.iter().map(|row| row.count).sum();

    if !include_account {
        if all_folders {
            let folders: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "folder": row.folder.as_deref().unwrap_or(""),
                        "count": row.count
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({"folders": folders, "total": total})
            );
        } else {
            let row = rows.first();
            println!(
                "{}",
                serde_json::json!({
                    "folder": row.and_then(|r| r.folder.as_deref()).unwrap_or(""),
                    "count": row.map(|r| r.count).unwrap_or(0)
                })
            );
        }
        return;
    }

    if all_folders {
        let account_values: Vec<serde_json::Value> = accounts
            .iter()
            .map(|account| {
                let account_rows: Vec<&CountRow> = rows
                    .iter()
                    .filter(|row| row.account == account.name)
                    .collect();
                let folders: Vec<serde_json::Value> = account_rows
                    .iter()
                    .map(|row| {
                        serde_json::json!({
                            "folder": row.folder.as_deref().unwrap_or(""),
                            "count": row.count
                        })
                    })
                    .collect();
                let account_total: usize = account_rows.iter().map(|row| row.count).sum();
                serde_json::json!({
                    "account": account.label(),
                    "folders": folders,
                    "total": account_total
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"accounts": account_values, "total": total})
        );
    } else {
        let account_values: Vec<serde_json::Value> = accounts
            .iter()
            .map(|account| {
                let count = rows
                    .iter()
                    .find(|row| row.account == account.name)
                    .map(|row| row.count)
                    .unwrap_or(0);
                serde_json::json!({"account": account.label(), "count": count})
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"accounts": account_values, "total": total})
        );
    }
}

fn display_count_text(rows: &[CountRow], all_folders: bool, include_account: bool) {
    let total: usize = rows.iter().map(|row| row.count).sum();

    if !include_account {
        if rows.is_empty() {
            println!("0 message(s) match.");
        } else if all_folders {
            for row in rows {
                println!(
                    "{} message(s) in {}",
                    row.count,
                    row.folder.as_deref().unwrap_or("")
                );
            }
            if rows.len() > 1 {
                println!("{total} message(s) total");
            }
        } else if let Some(row) = rows.first() {
            println!(
                "{} message(s) in {}",
                row.count,
                row.folder.as_deref().unwrap_or("")
            );
        }
        return;
    }

    if rows.is_empty() {
        println!("0 message(s) match.");
        return;
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    let mut header = vec!["Account", "Count"];
    if all_folders {
        header.insert(1, "Folder");
    }
    table.set_header(header);

    for row in rows {
        let mut cells = vec![Cell::new(row.account.as_deref().unwrap_or(""))];
        if all_folders {
            cells.push(Cell::new(row.folder.as_deref().unwrap_or("")));
        }
        cells.push(Cell::new(row.count));
        table.add_row(cells);
    }

    if rows.len() > 1 {
        let mut total_row = vec![Cell::new("Total").fg(Color::Cyan)];
        if all_folders {
            total_row.push(Cell::new("").fg(Color::Cyan));
        }
        total_row.push(Cell::new(total).fg(Color::Cyan));
        table.add_row(total_row);
    }

    println!("{table}");
}

fn cmd_count_accounts(accounts: &[config::ResolvedAccount], args: &CountArgs) -> Result<()> {
    let mut rows = Vec::new();
    for account in accounts {
        let mut account_rows = with_account_session(account, |session| {
            let sp = spinner(&format!("Counting {}...", account.label()));
            let result = count_rows_for_account(session, account, args);
            sp.finish_and_clear();
            result
        })?;
        rows.append(&mut account_rows);
    }

    let include_account =
        accounts.len() > 1 || accounts.iter().any(|account| account.name.is_some());
    if args.json {
        display_count_json(accounts, &rows, args.filter.all_folders, include_account);
    } else {
        display_count_text(&rows, args.filter.all_folders, include_account);
    }
    Ok(())
}

fn cmd_search_accounts(accounts: &[config::ResolvedAccount], args: &SearchArgs) -> Result<()> {
    let messages = search_accounts(accounts, &args.filter, args.limit)?;
    if args.json {
        display::display_messages_json(&messages);
    } else {
        display::display_messages(&messages);
    }
    Ok(())
}

fn cmd_read_accounts(accounts: &[config::ResolvedAccount], args: &ReadArgs) -> Result<()> {
    let limit = args.limit.or(Some(1));
    let mut messages = Vec::new();
    let mut defaults = read::DefaultFolderMap::new();
    let mut bodies = read::MessageBodyMap::new();

    for account in accounts {
        let (mut account_messages, fetched, folder) = with_account_session(account, |session| {
            let criteria = args.filter.to_criteria(limit, &account.default_folder);

            let sp = spinner(&format!("Searching {}...", account.label()));
            let result = search::search(session, &criteria);
            sp.finish_and_clear();
            let mut account_messages = result?;

            tag_messages(&mut account_messages, account);

            let sp = spinner(&format!("Fetching from {}...", account.label()));
            let fetched = read::fetch_message_bodies(session, &account_messages, &criteria.folder);
            sp.finish_and_clear();

            Ok((account_messages, fetched?, criteria.folder))
        })?;

        for ((folder, uid), body) in fetched {
            bodies.insert((account.name.clone(), folder, uid), body);
        }
        defaults.insert(account.name.clone(), folder);
        messages.append(&mut account_messages);
    }

    sort_and_limit_messages(&mut messages, limit);
    if messages.is_empty() {
        println!("No messages found.");
        return Ok(());
    }

    read::print_messages_with_bodies(&messages, &defaults, &bodies);
    Ok(())
}

fn main() -> Result<()> {
    let matches = Cli::command().get_matches();
    let user_explicit = matches.value_source("user") == Some(ValueSource::CommandLine);
    let tls_explicit = matches.value_source("tls") == Some(ValueSource::CommandLine);
    let cli = Cli::from_arg_matches(&matches)?;

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

    reject_all_accounts_if_unsupported(&cli)?;

    let selector = if cli.all_accounts {
        config::AccountSelector::All
    } else if let Some(name) = cli.account.as_deref() {
        config::AccountSelector::Named(name)
    } else {
        config::AccountSelector::Default
    };
    let overrides = config::ConnectionOverrides {
        host: cli.host.clone(),
        port: cli.port,
        tls: if tls_explicit { Some(cli.tls) } else { None },
        user: cli.user.clone(),
        user_explicit,
    };
    let accounts = cfg.resolve_accounts(selector, &overrides)?;

    let result = match &cli.command {
        Commands::Draft(args) => cmd_draft(&accounts[0], args),
        Commands::Reply(args) => cmd_reply(&accounts[0], args),
        Commands::Search(args) => cmd_search_accounts(&accounts, args),
        Commands::Read(args) => cmd_read_accounts(&accounts, args),
        Commands::Delete(args) => {
            let account = &accounts[0];
            with_account_session(account, |session| {
                let criteria = args.filter.to_criteria(args.limit, &account.default_folder);
                let trash = args
                    .trash_folder
                    .as_deref()
                    .unwrap_or(&account.trash_folder);
                delete::delete_with_account(
                    session,
                    &criteria,
                    trash,
                    args.yes,
                    args.dry_run,
                    account.name.as_deref(),
                )
            })
        }
        Commands::Move(args) => {
            let account = &accounts[0];
            with_account_session(account, |session| {
                let criteria = args.filter.to_criteria(args.limit, &account.default_folder);
                delete::search_and_move_with_account(
                    session,
                    &criteria,
                    &args.to,
                    args.yes,
                    args.dry_run,
                    account.name.as_deref(),
                )
            })
        }
        Commands::Export(args) => {
            let account = &accounts[0];
            with_account_session(account, |session| {
                cmd_export(
                    session,
                    args,
                    &account.default_folder,
                    account.name.as_deref(),
                )
            })
        }
        Commands::Mark(args) => {
            let account = &accounts[0];
            with_account_session(account, |session| {
                cmd_mark(
                    session,
                    args,
                    &account.default_folder,
                    account.name.as_deref(),
                )
            })
        }
        Commands::Count(args) => cmd_count_accounts(&accounts, args),
        Commands::Quota => cmd_quota_accounts(&accounts),
        Commands::Status => cmd_status_accounts(&accounts),
        Commands::Completions { .. } | Commands::Manpage => unreachable!(),
    };

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_account() -> config::ResolvedAccount {
        config::ResolvedAccount {
            name: Some("work".to_string()),
            host: "imap.example.com".to_string(),
            port: 993,
            tls: true,
            user: "login".to_string(),
            pass_env: Some("WORK_PASS".to_string()),
            sender: Some("Me <me@example.com>".to_string()),
            drafts_folder: None,
            trash_folder: "Trash".to_string(),
            default_folder: "INBOX".to_string(),
        }
    }

    struct MustNotRead;

    impl Read for MustNotRead {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            panic!("stdin was read before credentials were resolved")
        }
    }

    #[test]
    fn draft_clap_accepts_repeated_single_mailbox_flags() {
        let cli = Cli::try_parse_from([
            "slashmail",
            "draft",
            "--to",
            "one@example.com",
            "--to",
            "\"Two, Person\" <two@example.com>",
            "--cc",
            "cc@example.com",
            "--bcc",
            "bcc@example.com",
            "--subject",
            "Hello",
            "--html",
            "--drafts-folder",
            "Nested/Drafts",
        ])
        .unwrap();
        let Commands::Draft(args) = cli.command else {
            panic!("expected draft command")
        };
        assert_eq!(args.to.len(), 2);
        assert_eq!(args.cc, ["cc@example.com"]);
        assert_eq!(args.bcc, ["bcc@example.com"]);
        assert!(args.html);
        assert_eq!(args.drafts_folder.as_deref(), Some("Nested/Drafts"));
    }

    #[test]
    fn draft_and_reply_clap_preserve_attachment_order_and_spaces() {
        let cli = Cli::try_parse_from([
            "slashmail",
            "draft",
            "--to",
            "one@example.com",
            "--attach",
            "first report.pdf",
            "--attach",
            "images/second image.PNG",
        ])
        .unwrap();
        let Commands::Draft(args) = cli.command else {
            panic!("expected draft command")
        };
        assert_eq!(
            args.attach,
            [
                PathBuf::from("first report.pdf"),
                PathBuf::from("images/second image.PNG"),
            ]
        );

        let cli = Cli::try_parse_from([
            "slashmail",
            "reply",
            "42",
            "--attach",
            "first report.pdf",
            "--attach",
            "images/second image.PNG",
        ])
        .unwrap();
        let Commands::Reply(args) = cli.command else {
            panic!("expected reply command")
        };
        assert_eq!(
            args.attach,
            [
                PathBuf::from("first report.pdf"),
                PathBuf::from("images/second image.PNG"),
            ]
        );
    }

    #[test]
    fn attachment_loader_keeps_empty_duplicate_and_unicode_files_in_order() {
        let directory = tempfile::tempdir().unwrap();
        let empty = directory.path().join("résumé.txt");
        let unknown = directory.path().join("payload.unknown-extension");
        std::fs::write(&empty, []).unwrap();
        std::fs::write(&unknown, [0_u8, 255, 17]).unwrap();

        let loaded = load_attachments(&[empty.clone(), unknown.clone(), empty.clone()]).unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].filename, "résumé.txt");
        assert!(loaded[0].bytes.is_empty());
        assert_eq!(
            loaded[0].content_type,
            lettre::message::header::ContentType::parse("text/plain").unwrap()
        );
        assert_eq!(loaded[1].filename, "payload.unknown-extension");
        assert_eq!(loaded[1].bytes, [0, 255, 17]);
        assert_eq!(
            loaded[1].content_type,
            lettre::message::header::ContentType::parse("application/octet-stream").unwrap()
        );
        assert_eq!(loaded[2], loaded[0]);
    }

    #[test]
    fn attachment_loader_infers_uppercase_extension_and_falls_back_without_one() {
        let directory = tempfile::tempdir().unwrap();
        let png = directory.path().join("image.PNG");
        let extensionless = directory.path().join("LICENSE");
        std::fs::write(&png, b"not actually a png").unwrap();
        std::fs::write(&extensionless, b"text").unwrap();

        let loaded = load_attachments(&[png, extensionless]).unwrap();

        assert_eq!(
            loaded[0].content_type,
            lettre::message::header::ContentType::parse("image/png").unwrap()
        );
        assert_eq!(
            loaded[1].content_type,
            lettre::message::header::ContentType::parse("application/octet-stream").unwrap()
        );
    }

    #[test]
    fn invalid_attachment_fails_after_credentials_but_before_stdin() {
        let account = draft_account();
        let missing = PathBuf::from("missing-attachment.txt");

        let error = match prepare_draft_with(
            &account,
            &[missing],
            |_| Some("secret".to_string()),
            MustNotRead,
        ) {
            Err(error) => error,
            Ok(_) => panic!("missing attachment should fail"),
        };

        assert!(error.to_string().contains("missing-attachment.txt"));
    }

    #[test]
    fn credential_failure_precedes_attachment_access() {
        let account = draft_account();
        let error = match prepare_draft_with(
            &account,
            &[PathBuf::from("missing-attachment.txt")],
            |_| None,
            MustNotRead,
        ) {
            Err(error) => error,
            Ok(_) => panic!("missing credentials should fail"),
        };

        assert!(error.to_string().contains("password environment"));
        assert!(!error.to_string().contains("missing-attachment.txt"));
    }

    #[test]
    fn attachment_names_reject_controls_bidi_and_line_separators_safely() {
        for name in [
            "escape\u{1b}[31m.txt",
            "looks-like-txt\u{202e}fdp",
            "multiline\u{2028}name.txt",
            "paragraph\u{2029}name.txt",
        ] {
            let error = load_attachments(&[PathBuf::from(name)]).unwrap_err();
            let rendered = error.to_string();
            assert!(rendered.contains("disallowed basename"));
            assert!(!rendered.chars().any(char::is_control));
            assert!(!rendered.contains('\u{202e}'));
            assert!(!rendered.contains('\u{2028}'));
            assert!(!rendered.contains('\u{2029}'));
            assert!(!rendered.contains("attachment contents"));
        }
    }

    #[test]
    fn directory_is_rejected_as_a_non_regular_attachment() {
        let directory = tempfile::tempdir().unwrap();
        let error = load_attachments(&[directory.path().to_path_buf()]).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_uses_caller_basename_and_rejects_dangling_or_directory_targets() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("resolved-secret-name.bin");
        let alias = directory.path().join("public-name.dat");
        std::fs::write(&target, [1_u8, 2, 3]).unwrap();
        symlink(&target, &alias).unwrap();

        let loaded = load_attachments(std::slice::from_ref(&alias)).unwrap();
        assert_eq!(loaded[0].filename, "public-name.dat");
        assert_eq!(loaded[0].bytes, [1, 2, 3]);

        let dangling = directory.path().join("dangling.txt");
        symlink(directory.path().join("does-not-exist"), &dangling).unwrap();
        assert!(load_attachments(&[dangling]).is_err());

        let directory_alias = directory.path().join("directory.txt");
        symlink(directory.path(), &directory_alias).unwrap();
        let error = load_attachments(&[directory_alias]).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_attachment_basename_is_rejected() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(OsString::from_vec(vec![b'a', 0xff]));
        std::fs::write(&path, b"data").unwrap();
        let error = load_attachments(&[path]).unwrap_err();
        assert!(error.to_string().contains("valid Unicode basename"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_attachment_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private.txt");
        std::fs::write(&path, b"private attachment bytes").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = load_attachments(std::slice::from_ref(&path));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err());
        assert!(!result
            .unwrap_err()
            .to_string()
            .contains("private attachment bytes"));
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_waiting_for_a_writer() {
        use std::process::Command;
        use std::sync::mpsc;

        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("pipe.dat");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender.send(load_attachments(&[fifo])).unwrap();
        });

        let result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("FIFO validation blocked waiting for a writer");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }

    #[test]
    fn reply_clap_has_only_the_confirmed_override_surface() {
        let cli = Cli::try_parse_from([
            "slashmail",
            "reply",
            "42",
            "--folder",
            "Archive",
            "--html",
            "--no-quote",
            "--drafts-folder",
            "Drafts",
        ])
        .unwrap();
        let Commands::Reply(args) = cli.command else {
            panic!("expected reply command")
        };
        assert_eq!(args.uid, 42);
        assert_eq!(args.folder.as_deref(), Some("Archive"));
        assert!(args.html);
        assert!(args.no_quote);
        assert!(
            Cli::try_parse_from(["slashmail", "reply", "42", "--subject", "override"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["slashmail", "reply", "42", "--to", "other@example.com"]).is_err()
        );
        assert!(Cli::try_parse_from(["slashmail", "reply", "0"]).is_err());
    }

    #[test]
    fn draft_and_reply_reject_all_accounts() {
        for command in [
            vec![
                "slashmail",
                "--all-accounts",
                "draft",
                "--to",
                "to@example.com",
            ],
            vec!["slashmail", "--all-accounts", "reply", "42"],
        ] {
            let cli = Cli::try_parse_from(command).unwrap();
            assert!(reject_all_accounts_if_unsupported(&cli).is_err());
        }
    }

    #[test]
    fn missing_or_empty_draft_credential_fails_before_stdin_read() {
        let account = draft_account();
        assert!(read_draft_body_with(&account, |_| None, MustNotRead).is_err());
        assert!(read_draft_body_with(&account, |_| Some(String::new()), MustNotRead).is_err());
    }

    #[test]
    fn draft_transport_requires_tls_before_stdin_for_remote_hosts() {
        let mut account = draft_account();
        account.tls = false;
        let error = read_draft_body_with(&account, |_| Some("secret".to_string()), MustNotRead)
            .err()
            .expect("remote plaintext draft should fail");
        assert!(error.to_string().contains("require TLS"));

        account.host = "127.0.0.1".to_string();
        let (_, body) =
            read_draft_body_with(&account, |_| Some("secret".to_string()), b"body".as_slice())
                .unwrap();
        assert_eq!(body, "body");
    }

    #[test]
    fn draft_body_preserves_unicode_and_uses_configured_environment_name() {
        let account = draft_account();
        let body = "Héllo 世界\nSecond line";
        let mut requested = String::new();
        let (credential, read) = read_draft_body_with(
            &account,
            |name| {
                requested = name.to_string();
                Some("secret".to_string())
            },
            body.as_bytes(),
        )
        .unwrap();
        assert_eq!(requested, "WORK_PASS");
        assert_eq!(credential.expose(), "secret");
        assert_eq!(read, body);
    }

    #[test]
    fn legacy_draft_credential_uses_slashmail_pass() {
        let mut account = draft_account();
        account.name = None;
        account.pass_env = None;
        account.user = "me@example.com".to_string();
        let mut requested = String::new();
        let credential = draft_credential_with(&account, |name| {
            requested = name.to_string();
            Some("secret".to_string())
        })
        .unwrap();
        assert_eq!(requested, "SLASHMAIL_PASS");
        assert_eq!(credential.expose(), "secret");
    }

    #[test]
    fn named_draft_account_requires_pass_env() {
        let mut account = draft_account();
        account.pass_env = None;
        assert!(draft_credential_with(&account, |_| Some("secret".to_string())).is_err());
    }

    #[test]
    fn sender_adaptation_prefers_config_and_requires_a_typed_fallback() {
        let account = draft_account();
        assert_eq!(
            draft_sender(&account).unwrap().email.to_string(),
            "me@example.com"
        );

        let mut fallback = account.clone();
        fallback.sender = None;
        fallback.user = "fallback@example.com".to_string();
        assert_eq!(
            draft_sender(&fallback).unwrap().email.to_string(),
            "fallback@example.com"
        );

        fallback.user = "not-a-mailbox".to_string();
        assert!(draft_sender(&fallback).is_err());
    }

    #[test]
    fn draft_failures_do_not_echo_credentials_body_or_mime() {
        let account = draft_account();
        let credential_error = read_draft_body_with(&account, |_| None, "private body".as_bytes())
            .err()
            .expect("missing credentials should fail");
        let rendered = credential_error.to_string();
        assert!(!rendered.contains("private body"));
        assert!(!rendered.contains("super-secret-password"));

        let composed = draft::compose_new_draft(draft::NewDraftInput {
            sender: draft::parse_mailbox("me@example.com", "sender").unwrap(),
            to: vec![draft::parse_mailbox("you@example.com", "To").unwrap()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Sensitive subject".to_string(),
            body: "private body".to_string(),
            format: draft::BodyFormat::Plain,
            attachments: Vec::new(),
        })
        .unwrap();
        for outcome in [
            draft::SaveOutcome::SavedUidUnresolved,
            draft::SaveOutcome::Unknown,
        ] {
            let error = report_save_outcome(outcome, &account, "Drafts", &composed).unwrap_err();
            let rendered = error.to_string();
            assert!(!rendered.contains("private body"));
            assert!(!rendered.contains("Content-Type"));
            assert!(!rendered.contains("super-secret-password"));
        }
    }

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
