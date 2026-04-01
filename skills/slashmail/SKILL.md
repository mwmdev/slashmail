---
name: slashmail
description: "Interact with email via the slashmail IMAP CLI. Use when the user asks to check email, search messages, read mail, delete emails, move messages between folders, mark messages read/unread/flagged, count emails, check mailbox quota, or any email-related task. Triggers on: email, mail, inbox, messages, check my email, search email, delete email, unread messages, slashmail."
---

# Slashmail

Email interaction via the `slashmail` CLI, an IMAP client.

**Prerequisites**: Verify `slashmail` is installed by running `which slashmail`. If not found, install from https://github.com/mwmdev/slashmail (Rust binary — `cargo install slashmail` or download from releases).

**Configuration**: Config file location is OS-dependent (Linux: `~/.config/slashmail/config.toml`, macOS: `~/Library/Application Support/slashmail/config.toml`, Windows: `%APPDATA%\slashmail\config.toml`). Password via env var `SLASHMAIL_PASS`. Pass it explicitly for non-interactive shells:

```bash
SLASHMAIL_PASS="$SLASHMAIL_PASS" slashmail <command>
```

## Filter Options (shared by all commands)

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

## Common Patterns

**Check inbox**: `slashmail search --limit 10`
**Unread count**: `slashmail count` (shows total in INBOX)
**Find emails from someone**: `slashmail search --from "name@example.com" --limit 20`
**Recent emails**: `slashmail search --since 1d --limit 20`
**Mailbox overview**: `slashmail status`
**Search email content**: `slashmail search --body "invoice" --since 1m`
**Search everywhere**: `slashmail search --text "quarterly report"`
**Read a message**: `slashmail read --from "boss@example.com" --limit 1`
**Clean up old newsletters**: `slashmail delete --from "newsletter@" --before 3m --dry-run` then confirm with user before running without `--dry-run`
