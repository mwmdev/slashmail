# slashmail

[![CI](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml/badge.svg)](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/slashmail)](https://crates.io/crates/slashmail)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue)](https://www.rust-lang.org)
[![Crate Size](https://img.shields.io/crates/size/slashmail)](https://crates.io/crates/slashmail)
[![License](https://img.shields.io/crates/l/slashmail)](LICENSE-MIT)

CLI for searching, managing, drafting, and bulk-operating on emails via IMAP.

## Install

### From crates.io

```bash
cargo install slashmail
```

### From GitHub Releases

Download a prebuilt binary from [Releases](https://github.com/mwmdev/slashmail/releases/latest), extract it, and place it on your `PATH`.

### From source

Requires [Rust](https://rustup.rs/) and a C compiler (for OpenSSL bindings).

```bash
git clone https://github.com/mwmdev/slashmail.git
cd slashmail
cargo build --release
cp target/release/slashmail ~/.local/bin/   # or anywhere on your PATH
```

If OpenSSL cannot be discovered on your system, build with
`cargo build --release --features vendored-openssl`.

#### Platform notes

| OS | Prerequisites |
|---|---|
| **macOS** | Xcode Command Line Tools (`xcode-select --install`) |
| **Debian/Ubuntu** | `apt install build-essential pkg-config libssl-dev` |
| **Fedora/RHEL** | `dnf install gcc pkg-config openssl-devel` |
| **Arch** | `pacman -S base-devel openssl` |
| **NixOS** | `nix-shell` (uses included `shell.nix`) |
| **Windows** | Install Rust via [rustup](https://rustup.rs/), uses vendored OpenSSL |

## Usage

```
slashmail [OPTIONS] <COMMAND>

Commands:
  draft    Save a new unsent email draft
  reply    Save an unsent reply draft for one message UID
  attachments  List or save attachments from one message UID
  search   Search messages by criteria
  read     Display the content of matching messages
  delete   Search + delete matching messages (move to Trash)
  move     Search + move matching messages to a folder
  export   Search + export matching messages as .eml files
  mark     Search + set/unset flags on matching messages
  count    Count matching messages (no header or body fetch)
  quota    Show mailbox quota usage
  status   Show per-folder message statistics
```

### Connection options

```
--host <HOST>      IMAP host [default: 127.0.0.1]
--port <PORT>      IMAP port [default: 1143 plain, 993 TLS]
--tls              Use TLS (required for every non-loopback IMAP host)
-u, --user <USER>  IMAP username (or SLASHMAIL_USER env)
--account <NAME>   Use a named account from config
--all-accounts     Query all configured accounts (read-only commands only)
```

For direct/legacy connections, the password is read from `SLASHMAIL_PASS` or prompted interactively. Named accounts use the environment variable named by their `pass_env` setting.

When a config file is loaded, slashmail automatically loads `.env` from the same directory. Values already present in the process environment take precedence. A missing `.env` is ignored; an unreadable or malformed one is an error. Because `.env` contains plaintext secrets, keep it out of version control and readable only by your user account.

`draft` and `reply` are different because stdin is reserved for the message body: they never prompt for a password. Direct/legacy use requires a nonempty `SLASHMAIL_PASS`. A named account must configure `pass_env`, and that variable must be nonempty in the process environment or adjacent `.env`. Slashmail checks credentials before reading stdin, so a missing password cannot consume a piped draft body.

Connection options are global and can appear before or after the subcommand.

### Config file

Settings can be stored in a config file to avoid repeating connection options:

| OS | Path |
|---|---|
| **Linux** | `~/.config/slashmail/config.toml` |
| **macOS** | `~/Library/Application Support/slashmail/config.toml` |
| **Windows** | `%APPDATA%\slashmail\config.toml` |

Single-account `config.toml`:

```toml
host = "imap.gmail.com"
port = 993
tls = true
user = "user@gmail.com"
sender = "User Example <user@gmail.com>"
drafts_folder = "[Gmail]/Drafts"
trash_folder = "[Gmail]/Trash"
default_folder = "INBOX"
```

All single-account fields are optional. CLI arguments and environment variables take precedence over these top-level config values.

Multi-account `config.toml`:

```toml
default_account = "personal"

[[accounts]]
name = "personal"
host = "imap.gmail.com"
port = 993
tls = true
user = "user@gmail.com"
pass_env = "SLASHMAIL_PERSONAL_PASS"
sender = "Personal User <user@gmail.com>"
drafts_folder = "[Gmail]/Drafts"
trash_folder = "[Gmail]/Trash"
default_folder = "INBOX"

[[accounts]]
name = "work"
host = "imap.fastmail.com"
port = 993
tls = true
user = "user@company.com"
pass_env = "SLASHMAIL_WORK_PASS"
sender = "Work User <user@company.com>"
drafts_folder = "Drafts"
default_folder = "INBOX"
```

Create `.env` beside `config.toml` with the variables named by each account's `pass_env`:

```dotenv
SLASHMAIL_PERSONAL_PASS=your-personal-password
SLASHMAIL_WORK_PASS=your-work-password
```

When `[[accounts]]` is configured, slashmail uses `default_account` by default, or the first account if `default_account` is omitted. Use `--account <NAME>` to select one account, or `--all-accounts` to aggregate read-only commands across every account.

`--all-accounts` is supported for `search`, `read` (without `--uid`), `count`, `status`, and `quota`. Mutating commands (`delete`, `move`, `mark`), `export`, drafts, replies, and received-attachment inspection require a single account.

Use `--config <PATH>` to specify an alternative config file location.

`sender` and `drafts_folder` are optional at both the top level and inside an `[[accounts]]` entry. An account value takes precedence over the top-level value. Draft composition falls back to `user` only when it is a valid email mailbox; configure `sender` when the IMAP login is not an email address.

The draft destination is resolved in this order: command `--drafts-folder`, the selected account's resolved `drafts_folder` (account value, then top-level value), then exactly one selectable server mailbox marked `\Drafts`. Slashmail fails without saving if the chosen override is invalid or server discovery finds zero or multiple valid Drafts mailboxes.

### Email drafts

`draft` and `reply` save unsent messages with the IMAP `\Draft` flag; they do
not send mail. The body is read from stdin and is plain text unless `--html`
is used. Repeat `--to`, `--cc`, `--bcc`, or `--attach` to add multiple
recipients or local files.

```bash
# Create a draft with an attachment
printf '%s\n' 'Please review the attached proposal.' |
  slashmail draft --account work \
    --to client@example.com \
    --subject "Proposal" \
    --attach './documents/client proposal.pdf'

# Reply to UID 1842 without quoting the original message
printf '%s\n' 'Thanks, this looks good to me.' |
  slashmail reply --account work --no-quote 1842

# Create an HTML draft
printf '%s\n' '<p>Please review the <strong>proposal</strong>.</p>' |
  slashmail draft --html --to client@example.com --subject "Proposal"
```

Replies use reply-all behavior, exclude the configured sender, preserve
available thread metadata, and quote the original by default. Use `--folder`
to select the source folder and `--no-quote` to omit the quote. The source
message remains unchanged.

Each `--attach` value must name a local regular file. Globs, directories, and
URLs are not supported, and attachments from the original message are not
copied into replies. Files are loaded into memory while the MIME message is
built, so large attachments may be limited by available memory or the mailbox
provider.

On success, slashmail prints the account, Drafts folder, UID, recipients, and
subject. Add `--json` to print the same receipt, plus the draft's
`message_id`, as one JSON object. This receipt may contain Bcc addresses, so
avoid copying it into public logs. If the APPEND outcome is reported as
unknown, inspect the Drafts folder before retrying to avoid creating a
duplicate.

### Received attachments

`attachments <UID>` lists attachments from one message without marking it as
seen. The output includes MIME part IDs, filenames, content types, and decoded
sizes; add `--json` for machine-readable output.

```bash
# List attachments
slashmail attachments --account work 1842

# Save every attachment
slashmail attachments --account work --save \
  --output-dir './received files' 1842

# Save selected MIME parts
slashmail attachments --account work --save \
  --part 2.1 --part 3 --output-dir './received files' 1842
```

Saving aborts before writing anything if a destination already exists, and
stops rather than overwrite an earlier part when two names resolve to the same
file (case- or accent-insensitive filesystems). Add
`--force` to replace existing files. Filenames are sanitized and kept inside
the output directory. Only parts declared as attachments are exposed;
inline/CID parts and attachments nested inside another attached message are
not extracted.

### Reading one message by UID

`read --uid <UID>` displays exactly that message from the selected folder
instead of the newest filter match. The folder is `--folder`, or else the
account's configured `default_folder` (INBOX if unset); UIDs are only unique
within one folder, so pass `--folder` explicitly when reusing a UID from
`search`. It fails when no message with that UID exists and cannot be combined
with `--all-folders` or `--all-accounts`. Like every read, it does not mark the
message as seen.

```bash
slashmail read --account work --folder INBOX --uid 1842
slashmail read --account work --folder INBOX --uid 1842 --json
```

`read --json` prints an array with one object per message: `uid`, `folder`,
`message_id`, `in_reply_to`, `references`, full `from`/`to`/`cc`/`date`/
`subject` headers, `timestamp`, `seen`/`answered`/`flagged`, the decoded text
`body`, and `attachments` (MIME `part`, `filename`, `content_type`, `size`).
Pass an attachment's `part` to `attachments --save --part`.

### JSON search fields

Each `search --json` row contains `uid`, `folder` (set with `--all-folders`),
`from`, `subject`, `date`, `timestamp`, `size`, the thread identifiers
`message_id` (or `null`), `in_reply_to`, and `references` (arrays of
angle-bracketed IDs), and the `seen`, `answered`, and `flagged` booleans.
Rows include `account` when a named account is used. `from`, `subject`, and
`date` are the full decoded values; only the terminal table shortens them.

```bash
# Messages you have not answered yet
slashmail search --since 7d --json |
  jq '.[] | select(.answered | not) | {uid, subject, message_id}'
```

### Filter options

Search, read, count, and bulk message commands share these filter options:

```
-f, --folder <FOLDER>    Folder to search [default: INBOX]
    --all-folders        Search across all folders (excludes Trash, Junk/Spam, All Mail, and folders inside them)
    --subject <TEXT>     Subject contains
    --from <TEXT>        From address contains
    --to <TEXT>          To address contains
    --cc <TEXT>          CC address contains
    --body <TEXT>        Message body contains
    --text <TEXT>        Headers or body contains
    --seen               Only read messages
    --unseen             Only unread messages
    --since <DATE>       Messages since date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    --before <DATE>      Messages before date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    --larger <SIZE>      Messages larger than N bytes (supports K/M suffix)
    --smaller <SIZE>     Messages smaller than N bytes (supports K/M suffix)
    --flagged            Only flagged/starred messages
    --unflagged          Only unflagged messages
    --answered           Only replied-to messages
    --draft              Only draft messages
-n, --limit <N>          Limit number of results
```

All filter criteria are AND'd together. Omitting all criteria matches all messages. Text filters (`--subject`, `--from`, `--to`, `--cc`, `--body`, `--text`) must not be empty, so an unset shell variable cannot turn a filter into "match everything". `--folder` and `--all-folders` cannot be combined.

`--all-folders` skips mailboxes the server marks `\All`, `\Trash`, or `\Junk` (for example `Deleted Items` and `Junk Email`), plus folders named `Trash`, `Spam`, `Junk`, or `All Mail` for servers without those markers, and every folder inside one of those (such as `Deleted Items/2024` or `[Gmail]/Trash/Old`). It also skips containers that cannot be opened (`\Noselect`, such as Gmail's `[Gmail]`) but still searches the folders inside them. `delete` and `move` also never search their destination folder, and naming the destination as the only source folder is an error.

On Gmail, where every label is a folder, `--all-folders` lists a message once even when it has several labels: the INBOX copy if there is one, otherwise the copy in the first folder listed. `read`, `export`, `mark`, `move`, and `delete` act on that copy. `count --all-folders` still shows each folder's own count, but its total counts each message once.

On Proton Mail Bridge, every message sits in exactly one regular folder (INBOX, Archive, Sent, `Folders/...`), and its `Labels/...` folders and `Starred` are views of those messages. `--all-folders` skips those views there, so each message is listed and counted once, from its regular folder.

Servers list non-ASCII folder names in IMAP's modified UTF-7 (`[Gmail]/Messages envoy&AOk-s`). The terminal shows them decoded (`[Gmail]/Messages envoyés`), and every folder option and config setting accepts either form. `--json` output keeps the server's form.

### Action options

Commands that modify messages (`delete`, `move`, `mark`) support:

```
--yes       Skip confirmation prompt
--dry-run   Show what would happen without acting
```

`delete` also supports `--trash-folder <NAME>` (default: `Trash`) for servers that use a different name (e.g. `Deleted Items`, `[Gmail]/Trash`).

`move` requires `--dest <FOLDER>`; `--to` stays the recipient filter, as in every other command.

`delete` and `move` require the server to advertise `MOVE` or `UIDPLUS`. Without `MOVE`, messages are copied, flagged `\Deleted`, and removed with `UID EXPUNGE` of exactly those UIDs; other messages already flagged `\Deleted` are never expunged. This fallback is not atomic: if a step fails, slashmail stops and reports it without retrying, including how many messages were already moved or updated. Every mutating command and `export`/`read` refuse to act if the folder's `UIDVALIDITY` changed since the search. Immediately before `delete`, `move`, and `mark` act, slashmail asks the server which searched messages still exist; their receipts count only those and report any another client removed in the meantime.

`export` supports `--yes`, `--force` (replace existing files), and `-o, --output-dir`. Files are named `<folder>_<uid>.eml`, where `<folder>` is the server's listed name (`INBOX` even for `--folder inbox`, and non-ASCII names in their encoded form), percent-encoded: ASCII letters, digits, and `-` are kept and every other byte becomes `%XX` (so on Linux and macOS `Work/Projects` is `Work%2FProjects_1.eml` and `Work_Projects` is `Work%5FProjects_1.eml`). On Windows, lowercase letters are also encoded so folders differing only by case stay distinct (`Work/P` is `W%6F%72%6B%2FP_1.eml`). Without `--force`, an existing file is skipped only when it already holds the same message (identical bytes or the same Message-ID). UIDs restart when a mailbox is recreated or migrated, so an existing file holding a different message is left unchanged and reported as an error after the other messages are exported; use `--force` or a new output directory. `--force` replaces only a regular file or symlink entry and never follows symlinks. New exports and saved attachments are created owner-only (`0600`) on Unix.

`mark` takes one or more actions: `--read`, `--unread`, `--set-flagged`, `--clear-flagged`.

Search terms containing non-ASCII text are sent as UTF-8 literals and require the server to advertise `LITERAL+`, or `LITERAL-` (as Gmail does) for terms up to 4096 bytes; otherwise the search fails before any mailbox is searched. Proton Mail Bridge advertises neither, so non-ASCII search fails there. Bridge also matches `--subject` and `--text` against the raw message as stored, so words inside an encoded subject (common when it has non-ASCII characters, often base64) may not match, accented or not. To find such messages there, narrow with other filters (`--from`, `--since`) and check the decoded `subject` in `search --json`.

## Examples

```bash
# Search INBOX (all messages, newest first)
slashmail search -u user@example.com

# Search with filters
slashmail search -u user@example.com --from "newsletter" --since 2025-01-01
slashmail search -u user@example.com --subject "invoice" --larger 1M

# Relative dates: last 7 days, 2 weeks, 3 months, 1 year
slashmail search -u user@example.com --since 7d
slashmail search -u user@example.com --since 3m --before 1m

# Show only the 10 most recent matches
slashmail search -u user@example.com --from "alerts" -n 10

# Filter by recipient or CC
slashmail search -u user@example.com --to "team@company.com"
slashmail search -u user@example.com --cc "me@example.com"

# Show only unread messages
slashmail search -u user@example.com --unseen --since 7d

# Search message body content
slashmail search -u user@example.com --body "invoice attached"

# Search everywhere (headers + body)
slashmail search -u user@example.com --text "quarterly report"

# JSON output for scripting
slashmail search -u user@example.com --from "alerts" --json | jq '.[].subject'
slashmail count -u user@example.com --json

# Search across all folders
slashmail search -u user@example.com --all-folders --from "noreply"

# Search across all configured accounts
slashmail search --all-accounts --from "newsletter"
slashmail read --all-accounts --subject "invoice" -n 3

# Use one named account from config
slashmail count --account work --unseen

# Delete with interactive confirmation
slashmail delete -u user@example.com --from "spam@example.com"

# Batch delete (no prompt)
slashmail delete -u user@example.com --subject "unsubscribe" --yes

# Preview what would be deleted
slashmail delete -u user@example.com --from "old-list" --dry-run

# Move messages to a folder
slashmail move -u user@example.com --from "receipts" --dest Archive

# Export messages as .eml files
slashmail export -u user@example.com --subject "contract" -o ./backup

# Mark messages as read
slashmail mark -u user@example.com --from "notifications" --read

# Flag important messages
slashmail mark -u user@example.com --subject "urgent" --set-flagged

# Count matching messages (fast, no header or body fetch)
slashmail count -u user@example.com --from "newsletter"

# Show folder statistics
slashmail status -u user@example.com

# Show mailbox quota
slashmail quota -u user@example.com

# Use with a remote IMAP server (Gmail, Fastmail, etc.)
slashmail search --tls --host imap.gmail.com -u user@gmail.com

# Use env vars to avoid typing credentials
export SLASHMAIL_USER=user@example.com
export SLASHMAIL_PASS=app-password
slashmail status
```

### Shell completions

```bash
# Bash
slashmail completions bash > ~/.local/share/bash-completion/completions/slashmail

# Zsh
slashmail completions zsh > ~/.zfunc/_slashmail

# Fish
slashmail completions fish > ~/.config/fish/completions/slashmail.fish
```

## Safety

- **Nothing is deleted permanently.** `delete` moves messages to the trash folder. `delete` and `move` remove a message from its folder only after the server has copied it to the destination, and no command expunges anything else. Your provider empties Trash on its own schedule (Gmail after 30 days).
- **Preview before acting.** `delete`, `move`, and `mark` list the matching messages first. `--dry-run` stops there; without `--yes`, the command asks before acting and the answer defaults to no.
- **Filters decide what changes.** Omitting every filter matches every message in the folder, and with `--all-folders` every message in every searched folder: `slashmail delete --yes` moves the whole INBOX to Trash. Check the count before you confirm.
- **`mark` can't be undone selectively.** After a bulk `mark --read`, which messages were unread is lost. Preview with `--dry-run`.
- **Stale results are refused.** If a folder's `UIDVALIDITY` changed since the search, `delete`, `move`, `mark`, `export`, and `read` stop instead of acting on other messages.
- **Back up first.** Before a bulk change, `export` the messages to `.eml` files. `export` and `attachments --save` never overwrite a file without `--force`.

## AI Agent Skill

slashmail includes a skill file (`skills/slashmail/SKILL.md`) that teaches AI agents how to manage your email through natural language.

Install it with the [skills CLI](https://www.skills.sh/docs/cli) (Claude Code, Codex, Cursor, and more; set `DISABLE_TELEMETRY=1` to opt out of its telemetry):

```bash
npx skills add mwmdev/slashmail
```

Then ask your agent to "set up slashmail for my Gmail account". The skill installs the binary if needed, writes `config.toml`, and asks you to put your password in `.env` yourself.

To install by hand instead, **Claude Code** — copy the skill into your skills directory:

```bash
mkdir -p ~/.claude/skills/slashmail
cp skills/slashmail/SKILL.md ~/.claude/skills/slashmail/
```

**Codex** — copy the skill into the shared agent skills directory:

```bash
mkdir -p ~/.agents/skills/slashmail
cp skills/slashmail/SKILL.md ~/.agents/skills/slashmail/
```

**Other agents** — paste the contents of `skills/slashmail/SKILL.md` into your agent's system prompt or tool definitions.

Once installed, prompts like these just work:

```
> Check my latest emails
> Read the last email from Sarah
> Find emails about the quarterly report
> How many unread messages do I have?
> Show me large emails over 5MB from the last month
> Delete all newsletters from noreply@example.com older than 3 months
> Move flagged emails from last week to the Archive folder
> Export all invoices from 2025 to a backup folder
> Search my sent folder for emails to the finance team
> Draft a plain-text email to Sarah with subject "Project update"
> Save an HTML reply to message UID 1842 without quoting the original
```

Destructive operations always dry-run first and ask for confirmation.

## Tested with

- Gmail (`--tls --host imap.gmail.com`, with an app password)
- Proton Mail Bridge 3.23 (`127.0.0.1:1143`, read-only checks)
- Dovecot 2.3
- GreenMail (automated test suite)

Other IMAP4rev1 servers should work. `delete` and `move` need `MOVE` or `UIDPLUS`, and non-ASCII search needs `LITERAL+` or `LITERAL-`.

## How it works

- All filtering runs server-side via IMAP SEARCH
- Uses IMAP SORT extension (RFC 5256) when available for a single folder. Otherwise, and for every folder of `--all-folders` and every account of `--all-accounts`, slashmail orders by the Date header, but never later than one day after a message arrived, so a wrong or forged future date cannot keep a message at the top
- `--limit` keeps header fetches small. With SORT, a single-folder search is truncated before fetching. Otherwise (no SORT, as on Gmail and Proton Bridge, and each folder or account being merged), a large search is first narrowed to recently arrived messages with `SINCE`, and every match is fetched only when those hold too few. The result is the same as fetching everything
- `search`, `delete`, `move`, `mark` only fetch headers and size -- never full messages; `count` fetches neither (only message IDs with `--all-folders` on Gmail)
- `export` fetches full message bodies via `BODY.PEEK[]`
- Uses `BODY.PEEK` to avoid marking messages as read, and opens folders read-only (`EXAMINE`) for `search`, `read`, `count`, `export`, and `--dry-run`, so they do not clear the `\Recent` flag
- UID sets are compressed into ranges and chunked to stay within IMAP command length limits
- Message content, headers, folder names, and server errors are rendered inert in the terminal: escape sequences and control characters are removed, and invisible formatting characters (such as zero-width spaces) are shown as spaces in names and dropped from bodies, so look-alike names stay distinguishable. `--json` output and exported `.eml` files keep the original data.
- The password buffer slashmail reads is zeroed after login. Copies held by the process environment, `.env` loading, or the IMAP library's LOGIN command are not guaranteed to be wiped.

## Exit codes

- `0` — Success
- `1` — Error (connection failure, invalid credentials, refused operation, etc.)
- `2` — Invalid command-line usage

All errors print to stderr. Combine `--yes` with cron or scripts for unattended operation.

## Troubleshooting

### Connection refused

- Verify host and port: ProtonMail Bridge uses `127.0.0.1:1143`, Gmail uses `imap.gmail.com:993 --tls`
- Check that the IMAP server is running and the port is not blocked by a firewall

### Login failed

- Gmail requires an [App Password](https://support.google.com/accounts/answer/185833), not your account password
- Outlook.com and Microsoft 365 no longer accept passwords over IMAP (OAuth only), so they are not supported yet
- ProtonMail Bridge: use the bridge-generated password, not your ProtonMail account password
- Fastmail: use an app-specific password from Settings → Privacy & Security

### Folder not found

- Run `slashmail status` to list all available folders and their names
- `search`, `read`, `export`, `mark`, `move`, and `delete` need a name `slashmail status` lists, decoded as shown or as the server sends it (INBOX in any letter case). `count`, `reply`, and `attachments` also try a name exactly as typed, for mailboxes a server opens but does not list
- Gmail uses `[Gmail]/Trash`, `[Gmail]/All Mail`, etc. — use `--trash-folder` with `delete` if needed
- Exchange/Outlook uses `Deleted Items` instead of `Trash`

### TLS errors

- `--tls` is required for all non-loopback IMAP hosts; plaintext is only allowed for `localhost` and loopback addresses (for example ProtonMail Bridge)
- If you get certificate errors, ensure your system CA certificates are up to date

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option. slashmail is free software, provided as is, without warranty.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in slashmail by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
