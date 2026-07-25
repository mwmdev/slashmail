# slashmail

[![CI](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml/badge.svg)](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/slashmail)](https://crates.io/crates/slashmail)
[![MSRV](https://img.shields.io/badge/MSRV-1.83-blue)](https://www.rust-lang.org)
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
  search   Search messages by criteria
  read     Display the content of matching messages
  delete   Search + delete matching messages (move to Trash)
  move     Search + move matching messages to a folder
  export   Search + export matching messages as .eml files
  mark     Search + set/unset flags on matching messages
  count    Count matching messages (no FETCH)
  quota    Show mailbox quota usage
  status   Show per-folder message statistics
```

### Connection options

```
--host <HOST>      IMAP host [default: 127.0.0.1]
--port <PORT>      IMAP port [default: 1143 plain, 993 TLS]
--tls              Use TLS (required for remote IMAP servers)
-u, --user <USER>  IMAP username (or SLASHMAIL_USER env)
--account <NAME>   Use a named account from config
--all-accounts     Query all configured accounts (read-only commands only)
```

For direct/legacy connections, the password is read from `SLASHMAIL_PASS` env var or prompted interactively.
For named accounts, set `pass_env` per account or slashmail prompts for that account.

`draft` and `reply` are different because stdin is reserved for the message body: they never prompt for a password. Direct/legacy use requires a nonempty `SLASHMAIL_PASS`. A named account must configure `pass_env`, and the environment variable named there must be nonempty. Slashmail checks credentials before reading stdin, so a missing password cannot consume a piped draft body.

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

When `[[accounts]]` is configured, slashmail uses `default_account` by default, or the first account if `default_account` is omitted. Use `--account <NAME>` to select one account, or `--all-accounts` to aggregate read-only commands across every account.

`--all-accounts` is supported for `search`, `read`, `count`, `status`, and `quota`. Mutating commands (`delete`, `move`, `mark`) and `export` require a single account.

Use `--config <PATH>` to specify an alternative config file location.

`sender` and `drafts_folder` are optional at both the top level and inside an `[[accounts]]` entry. An account value takes precedence over the top-level value. Draft composition falls back to `user` only when it is a valid email mailbox; configure `sender` when the IMAP login is not an email address.

The draft destination is resolved in this order: command `--drafts-folder`, the selected account's resolved `drafts_folder` (account value, then top-level value), then exactly one selectable server mailbox marked `\Drafts`. Slashmail fails without saving if the chosen override is invalid or server discovery finds zero or multiple valid Drafts mailboxes.

### Email drafts

`draft` and `reply` save an unsent message immediately with the IMAP `\Draft` flag. They do not send mail. The new body comes exclusively from stdin and is plain text unless `--html` is present.

Each `--to`, `--cc`, or `--bcc` occurrence accepts one RFC mailbox. Repeat the flag for multiple recipients; do not combine multiple mailboxes in one comma-separated value because a quoted display name can itself contain a comma.

```bash
# New plain-text draft
printf '%s\n' 'Please review the attached proposal.' |
  slashmail draft --to client@example.com --subject "Proposal"

# Multiple recipients and a named account
printf '%s\n' 'Here is the project update.' |
  slashmail draft --account work \
    --to 'Alice Example <alice@example.com>' \
    --to bob@example.com \
    --cc manager@example.com \
    --bcc archive@example.com \
    --subject "Project update"

# New HTML draft
printf '%s\n' '<p>Please review the <strong>proposal</strong>.</p>' |
  slashmail draft --html --to client@example.com --subject "Proposal"

# Save to an explicit destination instead of configured/server discovery
printf '%s\n' 'Draft body' |
  slashmail draft --to client@example.com --drafts-folder "Saved/Drafts"

# Reply to UID 1842 in the account's default folder
printf '%s\n' 'Thanks, this looks good to me.' |
  slashmail reply --account work 1842

# Reply to a UID in another source folder and omit the original quote
printf '%s\n' 'Following up with one correction.' |
  slashmail reply --account work --folder Archive --no-quote 1842

# HTML reply saved to an explicit Drafts destination
printf '%s\n' '<p>Thanks, this looks good to me.</p>' |
  slashmail reply --account work --html \
    --drafts-folder "Saved/Drafts" 1842
```

A reply targets exactly one source message by selected account, source `--folder` (or that account's `default_folder`), and UID. Slashmail derives the subject and recipients automatically using reply-all behavior, excludes the configured sender, preserves available thread metadata, and quotes the decoded original by default. Use `--no-quote` to omit that quote. Saving a reply does not mark the source as seen or answered.

Confirmed success prints one stable line with these labeled fields:

```text
Draft saved: Account=work | Folder=Drafts | UID=1843 | To=alice@example.com | Cc=bob@example.com | Bcc= | Subject=Re: Project update
```

The receipt contains recipient metadata, including Bcc addresses, so avoid copying it into public logs. If slashmail reports that the draft was saved but its UID could not be resolved, or that the APPEND outcome is unknown, inspect the Drafts mailbox before retrying. An automatic retry could create a duplicate.

Draft support intentionally does not include sending, forwarding, attachments, raw MIME, sender aliases, arbitrary headers, editing existing drafts, or aggregate `--all-accounts` creation.

### Filter options

Search, read, count, and bulk message commands share these filter options:

```
-f, --folder <FOLDER>    Folder to search [default: INBOX]
    --all-folders        Search across all folders (excludes Trash, Spam)
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

All filter criteria are AND'd together. Omitting all criteria matches all messages.

### Action options

Commands that modify messages (`delete`, `move`, `mark`) support:

```
--yes       Skip confirmation prompt
--dry-run   Show what would happen without acting
```

`delete` also supports `--trash-folder <NAME>` (default: `Trash`) for servers that use a different name (e.g. `Deleted Items`, `[Gmail]/Trash`).

`export` supports `--yes`, `--force` (overwrite existing files), and `-o, --output-dir`.

`mark` takes one or more flags: `--read`, `--unread`, `--flagged`, `--unflagged`.

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

# JSON output for scripting (search and count only)
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
slashmail move -u user@example.com --from "receipts" --to Archive

# Export messages as .eml files
slashmail export -u user@example.com --subject "contract" -o ./backup

# Mark messages as read
slashmail mark -u user@example.com --from "notifications" --read

# Flag important messages
slashmail mark -u user@example.com --subject "urgent" --flagged

# Count matching messages (fast, no FETCH)
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

## AI Agent Skill

slashmail includes a skill file (`skills/slashmail/SKILL.md`) that teaches AI agents how to manage your email through natural language.

**Claude Code** — copy the skill into your skills directory:

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

## Releasing

Releases are tag-driven. The release workflow validates that the tag matches
the crate version, builds five platform archives, publishes to crates.io, and
only then creates the GitHub release.

1. Update `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` in a release-preparation
   pull request.
2. Run the same publishability checks used by CI:

   ```bash
   cargo test --locked --features vendored-openssl
   cargo package --locked --features vendored-openssl
   cargo publish --dry-run --locked --features vendored-openssl
   ```

3. Merge the pull request only after CI passes.
4. Tag the exact merged `origin/main` commit and push the tag:

   ```bash
   VERSION=0.5.0
   git fetch origin main --tags
   git show origin/main:Cargo.toml | grep "^version = \"${VERSION}\"$"
   git tag "v${VERSION}" origin/main
   git push origin "v${VERSION}"
   ```

5. Watch the `Release` workflow and verify the GitHub release has all five
   archives and crates.io lists the new version:

   ```bash
   run_id="$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
   gh run watch "${run_id}" --exit-status
   gh release view "v${VERSION}"
   cargo info "slashmail@${VERSION}"
   ```

The repository must have a valid `CARGO_REGISTRY_TOKEN` Actions secret. Never
push the release tag before the release-preparation commit is on `main`.

## Tested with

- Gmail (via `--tls --host imap.gmail.com`)
- Fastmail (via `--tls --host imap.fastmail.com`)
- Dovecot
- Any standard IMAP4rev1 server

## How it works

- All filtering runs server-side via IMAP SEARCH
- Uses IMAP SORT extension (RFC 5256) when available; falls back to client-side sort
- With SORT, `--limit` truncates results before fetching (fewer bytes over the wire)
- `search`, `delete`, `move`, `mark`, `count` only fetch headers and size -- never full messages
- `export` fetches full message bodies via `BODY.PEEK[]`
- Uses `BODY.PEEK` to avoid marking messages as read
- UID sets are compressed into ranges and chunked to stay within IMAP command length limits
- Passwords are securely zeroed from memory after login

## Exit codes

- `0` — Success
- `1` — Error (connection failure, invalid credentials, bad arguments, etc.)

All errors print to stderr. Combine `--yes` with cron or scripts for unattended operation.

## Troubleshooting

### Connection refused

- Verify host and port: ProtonMail Bridge uses `127.0.0.1:1143`, Gmail uses `imap.gmail.com:993 --tls`
- Check that the IMAP server is running and the port is not blocked by a firewall

### Login failed

- Gmail and Outlook require [App Passwords](https://support.google.com/accounts/answer/185833), not your account password
- ProtonMail Bridge: use the bridge-generated password, not your ProtonMail account password
- Fastmail: use an app-specific password from Settings → Privacy & Security

### Folder not found

- Run `slashmail status` to list all available folders and their names
- Folder names are case-sensitive on most IMAP servers
- Gmail uses `[Gmail]/Trash`, `[Gmail]/All Mail`, etc. — use `--trash-folder` with `delete` if needed
- Exchange/Outlook uses `Deleted Items` instead of `Trash`

### TLS errors

- Use `--tls` for all remote (non-localhost) IMAP servers
- If you get certificate errors, ensure your system CA certificates are up to date
