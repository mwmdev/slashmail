---
name: slashmail
description: "Interact with email via the slashmail IMAP CLI. Use when the user asks to draft, reply to, check, search, read, delete, move, mark, or count email, or check mailbox quota. Triggers on: email, mail, inbox, messages, draft email, reply to email, check my email, search email, delete email, unread messages, slashmail."
---

# Slashmail

Email interaction via the `slashmail` CLI, an IMAP client.

**Prerequisites**: Verify `slashmail` is installed with `command -v slashmail`. If not found, install from https://github.com/mwmdev/slashmail (Rust binary — `cargo install slashmail` or download from releases). Before drafting, run `slashmail draft --help`; before replying, run `slashmail reply --help`. If the required command is unavailable, stop, report that the installed binary is stale, and suggest upgrading from a release or with Cargo. Do not substitute an arbitrary development build.

**Configuration**: Config file location is OS-dependent (Linux: `~/.config/slashmail/config.toml`, macOS: `~/Library/Application Support/slashmail/config.toml`, Windows: `%APPDATA%\slashmail\config.toml`). Direct/legacy connections use `SLASHMAIL_PASS`. Named accounts can define `pass_env` and can be selected with `--account NAME`; read-only commands can use `--all-accounts`.

```bash
SLASHMAIL_PASS="$SLASHMAIL_PASS" slashmail <command>
```

For `draft` and `reply`, credentials must be noninteractive because stdin is exclusively the new message body. A direct/legacy account requires a nonempty `SLASHMAIL_PASS`. A named account must configure `pass_env`, and that named environment variable must be nonempty. Slashmail resolves credentials before reading stdin and never prompts for these commands.

Optional `sender` and `drafts_folder` values can be set at the top level or per named account. The account value wins, then the top-level value. If no `sender` is configured, slashmail uses `user` only when it is a valid email mailbox. Draft destination precedence is `--drafts-folder`, resolved account configuration, then exactly one selectable server mailbox marked `\Drafts`.

## Filter Options (search and mailbox-operation commands)

| Flag | Description |
|------|-------------|
| `-f, --folder FOLDER` | Target folder (default: INBOX) |
| `--all-folders` | Search all folders (excludes Trash, Spam) |
| `--subject TEXT` | Filter by subject |
| `--from TEXT` | Filter by sender |
| `--to TEXT` | Filter by recipient |
| `--cc TEXT` | Filter by CC |
| `--body TEXT` | Search message body |
| `--text TEXT` | Search headers and body |
| `--seen` / `--unseen` | Filter by read status |
| `--since DATE` | Messages after date |
| `--before DATE` | Messages before date |
| `--larger SIZE` | Minimum size (e.g., `1M`, `500K`) |
| `--smaller SIZE` | Maximum size (e.g., `1M`, `500K`) |
| `--flagged` / `--unflagged` | Filter by starred status |
| `--answered` | Only replied-to messages |
| `--draft` | Only draft messages |
| `-n, --limit N` | Cap results |

Date formats: `YYYY-MM-DD` or relative (`7d`, `2w`, `3m`, `1y`). All filters combine with AND logic.

## Commands

| Command | Description | Extra flags |
|---------|-------------|-------------|
| `draft` | Save a new unsent draft; body is read from stdin | repeatable `--to`, `--cc`, `--bcc`; `--subject`, `--html`, `--drafts-folder` |
| `reply UID` | Save an unsent reply-all draft; body is read from stdin | `--folder`, `--html`, `--no-quote`, `--drafts-folder` |
| `search` | Retrieve messages (sorted newest-first) | `--json` |
| `read` | Display message content in terminal | — |
| `count` | Fast count without fetching content | `--json` |
| `delete` | Move to Trash | `--trash-folder NAME`, `--dry-run`, `--yes` |
| `move` | Move to folder | `--to DEST`, `--dry-run`, `--yes` |
| `mark` | Set/unset flags | `--read/--unread`, `--flagged/--unflagged`, `--dry-run`, `--yes` |
| `export` | Save as `.eml` files | `-o DIR`, `--force`, `--yes` |
| `status` | Per-folder message stats | — |
| `quota` | Mailbox capacity usage | — |

## Safety Rules

- **Always `--dry-run` first** for delete, move, and bulk mark operations. Show the user what will be affected before executing.
- **Never pass `--yes`** without showing the dry-run results to the user first and getting confirmation.
- **Use `--limit`** when the user asks for "recent" or "latest" messages to avoid fetching everything.
- **Draft and reply save immediately but never send.** Do not describe a saved draft as sent.
- **Inspect Drafts before retrying an ambiguous save.** If slashmail says the draft was saved but its UID is unresolved, or the APPEND outcome is unknown, retrying may create a duplicate.
- **Treat receipts as sensitive.** The stable success line includes Account, Folder, UID, To, Cc, Bcc, and Subject; do not place recipient metadata in public logs.

## Draft and Reply Rules

- Pipe the new body through stdin. Do not pass it as a positional argument or build raw MIME.
- Plain text is the default. Add `--html` only when the supplied stdin body is HTML.
- For a new draft, pass at least one `--to`. Each `--to`, `--cc`, or `--bcc` occurrence is exactly one RFC mailbox; repeat flags for multiple recipients instead of comma-splitting.
- For a reply, select one account, source `--folder` (default: the account's configured default folder), and source UID. Slashmail derives reply-all recipients, subject, and thread headers; there is no sender-only or recipient-override mode.
- Replies quote the decoded original by default. Add `--no-quote` only when the user asks to omit it.
- Use `--account NAME` for a named account and `--drafts-folder NAME` only when the destination must override configuration and server discovery.
- Scope is save-only: no sending, forwarding, attachments, raw MIME, aliases, arbitrary headers, editing existing drafts, or `--all-accounts` drafting.

```bash
# New plain-text draft
printf '%s\n' 'Please review the proposal.' |
  slashmail draft --to client@example.com --subject "Proposal"

# New HTML draft using a named account
printf '%s\n' '<p>Please review the <strong>proposal</strong>.</p>' |
  slashmail draft --account work --html \
    --to client@example.com --cc manager@example.com \
    --subject "Proposal"

# Explicit Drafts destination
printf '%s\n' 'Draft body' |
  slashmail draft --to client@example.com --drafts-folder "Saved/Drafts"

# Reply-all to UID 1842 in INBOX/default folder, with the original quoted
printf '%s\n' 'Thanks, this looks good.' |
  slashmail reply --account work 1842

# Reply from another source folder without quoting
printf '%s\n' 'Following up with one correction.' |
  slashmail reply --account work --folder Archive --no-quote 1842

# HTML reply to an explicit Drafts destination
printf '%s\n' '<p>Thanks, this looks good.</p>' |
  slashmail reply --account work --html \
    --drafts-folder "Saved/Drafts" 1842
```

Confirmed saves print exactly one control-free receipt line:

```text
Draft saved: Account=work | Folder=Drafts | UID=1843 | To=alice@example.com | Cc=bob@example.com | Bcc= | Subject=Re: Project update
```

## Common Patterns

**Check inbox**: `slashmail search --limit 10`
**Unread count**: `slashmail count` (shows total in INBOX)
**Find emails from someone**: `slashmail search --from "name@example.com" --limit 20`
**Recent emails**: `slashmail search --since 1d --limit 20`
**Mailbox overview**: `slashmail status`
**Search email content**: `slashmail search --body "invoice" --since 1m`
**Search everywhere**: `slashmail search --text "quarterly report"`
**Search all accounts**: `slashmail search --all-accounts --text "quarterly report"`
**Read a message**: `slashmail read --from "boss@example.com" --limit 1`
**Draft a new message**: `printf '%s\n' 'Draft body' | slashmail draft --to recipient@example.com --subject "Subject"`
**Draft a reply**: `printf '%s\n' 'Reply body' | slashmail reply --folder INBOX 1842`
**Clean up old newsletters**: `slashmail delete --from "newsletter@" --before 3m --dry-run` then confirm with user before running without `--dry-run`
