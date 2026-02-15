# slashmail

[![CI](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml/badge.svg)](https://github.com/mwmdev/slashmail/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/slashmail)](https://crates.io/crates/slashmail)
[![License](https://img.shields.io/crates/l/slashmail)](LICENSE-MIT)

CLI for searching, managing, and bulk-operating on emails via IMAP.

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
  search   Search messages by criteria
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
```

Password is read from `SLASHMAIL_PASS` env var or prompted interactively.

Connection options are global and can appear before or after the subcommand.

### Config file

Settings can be stored in a config file to avoid repeating connection options:

| OS | Path |
|---|---|
| **Linux** | `~/.config/slashmail/config.toml` |
| **macOS** | `~/Library/Application Support/slashmail/config.toml` |
| **Windows** | `%APPDATA%\slashmail\config.toml` |

Example `config.toml`:

```toml
host = "imap.gmail.com"
port = 993
tls = true
user = "user@gmail.com"
trash_folder = "[Gmail]/Trash"
default_folder = "INBOX"
```

All fields are optional. CLI arguments and environment variables take precedence over config values.

Use `--config <PATH>` to specify an alternative config file location.

### Filter options

All commands that operate on messages share the same filter options:

```
-f, --folder <FOLDER>    Folder to search [default: INBOX]
    --all-folders        Search across all folders (excludes Trash, Spam)
    --subject <TEXT>     Subject contains
    --from <TEXT>        From address contains
    --to <TEXT>          To address contains
    --cc <TEXT>          CC address contains
    --seen               Only read messages
    --unseen             Only unread messages
    --since <DATE>       Messages since date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    --before <DATE>      Messages before date (YYYY-MM-DD or 7d, 2w, 3m, 1y)
    --larger <SIZE>      Messages larger than N bytes (supports K/M suffix)
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

# Search across all folders
slashmail search -u user@example.com --all-folders --from "noreply"

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
