---
name: slashmail
description: "Interact with email via the slashmail IMAP CLI. Use when the user asks to draft, reply to, check, search, read, delete, move, mark, count, export, list or save attachments, inspect mailbox status, or check quota. Triggers on: email, mail, inbox, messages, draft email, reply to email, email attachments, save attachments, check my email, search email, delete email, unread messages, slashmail."
---

# Slashmail

Email interaction via the `slashmail` CLI, an IMAP client.

**Prerequisites**: Verify `slashmail` is installed with `command -v slashmail`. If not found, install from https://github.com/mwmdev/slashmail (Rust binary — `cargo install slashmail` or download from releases). Before drafting, run `slashmail draft --help`; before replying, run `slashmail reply --help`; before listing or saving received attachments, run `slashmail attachments --help`. If the required command or documented flag is unavailable, stop, report that the installed binary is stale, and suggest upgrading from a release or with Cargo. Do not substitute an arbitrary development build.

**Configuration**: Config file location is OS-dependent (Linux: `~/.config/slashmail/config.toml`, macOS: `~/Library/Application Support/slashmail/config.toml`, Windows: `%APPDATA%\slashmail\config.toml`). Direct/legacy connections use `SLASHMAIL_PASS`. Named accounts define `pass_env` and can be selected with `--account NAME`. Only `search`, `read`, `count`, `status`, and `quota` support `--all-accounts`.

**First-time setup**: If no config file exists, set slashmail up for the user:
1. Ask for their email address and provider. Write `config.toml` with `host`, `tls = true`, and `user` (Gmail `imap.gmail.com`; Yahoo `imap.mail.yahoo.com`; iCloud `imap.mail.me.com`; Proton Bridge `host = "127.0.0.1"`, `port = 1143`, no TLS). Outlook.com and Microsoft 365 no longer accept passwords over IMAP and slashmail has no OAuth sign-in, so tell those users their account is not supported yet. Never overwrite an existing config.
2. Never ask for, accept, or write the password. Tell the user to create `.env` beside `config.toml` themselves containing `SLASHMAIL_PASS=their-password` (Gmail, Yahoo, and iCloud need an app password; Proton uses the Bridge password), and to keep it private.
3. When they confirm, run `slashmail status` and report the result.

Slashmail automatically loads `.env` beside the selected `config.toml` without overriding values already present in the process environment. Store each named account password under the variable named by `pass_env`. Keep `.env` private and out of version control.

```dotenv
SLASHMAIL_PERSONAL_PASS=your-password
```

For `draft` and `reply`, credentials must be noninteractive because stdin is exclusively the new message body. A direct/legacy account requires a nonempty `SLASHMAIL_PASS`. A named account must configure `pass_env`, and that variable must be nonempty in the process environment or adjacent `.env`. Slashmail resolves credentials before reading stdin and never prompts for these commands.

Optional `sender` and `drafts_folder` values can be set at the top level or per named account. The account value wins, then the top-level value. If no `sender` is configured, slashmail uses `user` only when it is a valid email mailbox. Draft destination precedence is `--drafts-folder`, resolved account configuration, then exactly one selectable server mailbox marked `\Drafts`. `trash_folder` (default `Trash`, used by `delete`) and `default_folder` (default `INBOX`) can be set the same way.

**Connection options** (global, before or after the subcommand): `--host HOST` (default `127.0.0.1`), `--port PORT` (default 1143 plain, 993 with TLS), `--tls`, `-u/--user USER` (or `SLASHMAIL_USER`), and `--config PATH`. Plaintext IMAP is refused for every non-loopback host, so pass `--tls` (or set `tls = true`) for any remote server; only loopback servers such as ProtonMail Bridge may use plain TCP.

## Filter Options (search and mailbox-operation commands)

| Flag | Description |
|------|-------------|
| `-f, --folder FOLDER` | Target folder (default: INBOX); cannot be combined with `--all-folders` |
| `--all-folders` | Search all folders (excludes Trash, Junk/Spam, All Mail and folders inside them, and the move/delete destination; on Proton Bridge also `Labels/...` and `Starred`; on Gmail each message is one row, from INBOX when it is there) |
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
| `draft` | Save a new unsent draft; body is read from stdin | repeatable `--to`, `--cc`, `--bcc`, `--attach PATH`; `--subject`, `--html`, `--drafts-folder`, `--json` |
| `reply UID` | Save an unsent reply-all draft; body is read from stdin | repeatable `--attach PATH`; `--folder`, `--html`, `--no-quote`, `--drafts-folder`, `--json` |
| `attachments UID` | List or save received attachments without marking the message seen | `--folder`, `--json`, `--save`, repeatable `--part PART`, `-o DIR`, `--force` |
| `search` | Retrieve messages (sorted newest-first); JSON rows include `message_id`, `in_reply_to`, `references`, `seen`, `answered`, `flagged` | `--json` |
| `read` | Display message content without marking it seen | `--uid UID` (exact message in `--folder`; single account), `--json` |
| `count` | Fast count without fetching content | `--json` |
| `delete` | Move to Trash | `--trash-folder NAME`, `--dry-run`, `--yes` |
| `move` | Move to folder | `--dest FOLDER` (required; `--to` filters by recipient), `--dry-run`, `--yes` |
| `mark` | Set/unset flags | `--read/--unread`, `--set-flagged/--clear-flagged`, `--dry-run`, `--yes` |
| `export` | Save as `.eml` files | `-o DIR`, `--force`, `--yes` |
| `status` | Per-folder message stats | — |
| `quota` | Mailbox capacity usage | — |
| `completions SHELL` | Generate shell completions | `bash`, `zsh`, `fish`, `powershell`, or `elvish` |

## Safety Rules

- **Always `--dry-run` first** for delete, move, and bulk mark operations. Show the user what will be affected before executing.
- **Never pass `--yes` to delete, move, or mark** without showing the matching dry-run results to the user first and getting confirmation.
- **Preview exports with equivalent `search` filters** before using noninteractive `export --yes`. Use `--force` only when the user explicitly authorizes replacing the displayed destination files.
- **Use `--limit`** when the user asks for "recent" or "latest" messages to avoid fetching everything.
- **Draft and reply save immediately but never send.** Do not describe a saved draft as sent.
- **Attach only exact user-authorized paths.** Pass each literal local file path in its own `--attach` occurrence. Never expand globs, search directories, auto-discover files, or substitute a similarly named file.
- **List received attachments before saving** unless the user already specified exact MIME part IDs. Confirm the output directory before `--save`, and use `--force` only after the user explicitly authorizes replacing existing destination files or symlinks.
- **Inspect Drafts before retrying an ambiguous save.** If slashmail says the draft was saved but its UID is unresolved, or the APPEND outcome is unknown, retrying may create a duplicate.
- **Treat receipts as sensitive.** The stable success line includes Account, Folder, UID, To, Cc, Bcc, and Subject; do not place recipient metadata in public logs.

## Draft and Reply Rules

- Pipe the new body through stdin. Do not pass it as a positional argument or build raw MIME.
- Plain text is the default. Add `--html` only when the supplied stdin body is HTML.
- For a new draft, pass at least one `--to`. Each `--to`, `--cc`, or `--bcc` occurrence is exactly one RFC mailbox; repeat flags for multiple recipients instead of comma-splitting.
- For a reply, select one account, source `--folder` (default: the account's configured default folder), and source UID. Slashmail derives reply-all recipients, subject, and thread headers; there is no sender-only or recipient-override mode.
- Repeat `--attach PATH` for each exact local regular file. Quote paths containing spaces or shell metacharacters. Symlinks to regular files are accepted and expose the caller-supplied symlink basename; non-Unicode, control-bearing, and unsafe Unicode display names are rejected.
- Attachments are eagerly snapshotted before stdin or IMAP work. Memory use can be several times their aggregate size, there is no fixed supported-size guarantee, and the mailbox provider may reject a large draft. Ordinary local failures abort before APPEND; do not promise recovery from process-level out-of-memory termination.
- A reply attaches only the explicitly supplied local files; it never copies attachments from the source message. Successful creation still performs exactly one APPEND. Receipts retain the existing Account, Folder, UID, To, Cc, Bcc, and Subject fields and do not list attachments.
- Replies quote the decoded original by default. Add `--no-quote` only when the user asks to omit it.
- Use `--account NAME` for a named account and `--drafts-folder NAME` only when the destination must override configuration and server discovery.
- Scope is save-only: no sending, forwarding, raw MIME, aliases, arbitrary headers, editing existing drafts, or `--all-accounts` drafting. Attachment support does not accept globs, recursive discovery, URLs, stdin-sourced files, inline/CID parts, custom names or MIME types, or source-message attachments.

```bash
# New plain-text draft
printf '%s\n' 'Please review the proposal.' |
  slashmail draft --to client@example.com --subject "Proposal" \
    --attach './documents/client proposal.pdf' \
    --attach './charts/revenue.png'

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
  slashmail reply --account work \
    --attach './documents/revised proposal.pdf' 1842

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

With `--json`, the receipt is one object with `account`, `folder`, `uid`, `message_id`, `to`, `cc`, `bcc`, and `subject`. Prefer it when recording the draft UID.

## Reading by UID

- Once `search` has identified a message, use `read --folder FOLDER --uid UID` rather than re-filtering by sender, subject, or date. It fails if the UID no longer exists in that folder, and it rejects `--all-folders` and `--all-accounts`.
- `read --json` returns an array of objects with full `from`/`to`/`cc`/`date`/`subject`, `message_id`, `in_reply_to`, `references`, flags, decoded `body`, and `attachments` (`part`, `filename`, `content_type`, `size`). Pass a `part` to `attachments --save --part`.
- In `search --json`, `from`, `subject`, and `date` are full values. Match duplicates and follow-ups by `message_id`, `in_reply_to`, and `references` (angle-bracketed IDs), and use `answered` to skip mail already replied to.
- Never pass an empty text filter (for example an unset variable as `--from`); slashmail rejects it rather than matching every message.
- If `export` reports existing files that hold a different message, do not add `--force` without the user's approval; suggest a new output directory instead.
- If an action on searched messages fails because the folder's UIDVALIDITY changed, the mailbox was rebuilt since the search: run the search again and act on the new UIDs. Never reuse the old ones.
- `delete` and `move` require server support for `MOVE` or `UIDPLUS` and fail before changing anything otherwise. Non-ASCII search terms require `LITERAL+`, or `LITERAL-` for terms up to 4096 bytes; without it the search fails rather than matching nothing. Report these failures instead of working around them.
- `--json` `folder` values are the server's names (non-ASCII names in modified UTF-7, such as `[Gmail]/Messages envoy&AOk-s`). Pass them back unchanged; folder options also accept the decoded name the terminal shows.
- With `--all-folders`, a Gmail message with several labels is one row, from INBOX when it is there, and commands act on that copy. Gmail keeps flags per message, so `mark` changes it in every label. On Proton Bridge, label folders are skipped: to work with a label, name it with `--folder` (for example `--folder "Labels/Clients"`).
- On Proton Bridge, server search cannot see words inside an encoded subject (common when it has non-ASCII characters): `--subject` and `--text` both match the raw message. Narrow with other filters (`--from`, `--since`) and check the decoded `subject` field of `search --json` yourself, and tell the user before reporting a Bridge subject search as complete.

## Received Attachment Rules

- Select one account, source `--folder` (default: the account's configured default folder), and positive source UID. The `attachments` command does not support `--all-accounts`.
- Listing is the default and does not mark the message as seen. Add `--json` only for machine-readable metadata; it conflicts with `--save`.
- Add `--save` to write all declared attachments, or repeat `--part PART` to save specific canonical MIME part IDs such as `2` or `2.1`. `--part`, `--output-dir`, and `--force` require `--save`.
- Prefer an explicit `--output-dir`; otherwise files are written to the current directory. Slashmail creates the output directory when needed.
- Slashmail sanitizes filenames, resolves same-message name collisions deterministically, and refuses existing destinations unless `--force` is supplied. With `--force`, it replaces regular files or symlinks but rejects other filesystem objects.
- Only MIME parts declared as attachments are exposed. Inline/CID parts and attachments nested inside another attached message are not extracted.

```bash
# List received attachments
slashmail attachments --account work --folder INBOX 1842

# Inspect attachment metadata as JSON
slashmail attachments --account work --folder INBOX --json 1842

# Save selected parts after reviewing the listing
slashmail attachments --account work --folder INBOX --save \
  --part 2.1 --part 3 --output-dir './received files' 1842
```

## Common Patterns

**Check inbox**: `slashmail search --limit 10`
**Unread count**: `slashmail count --unseen`
**Find emails from someone**: `slashmail search --from "name@example.com" --limit 20`
**Recent emails**: `slashmail search --since 1d --limit 20`
**Mailbox overview**: `slashmail status`
**Search email content**: `slashmail search --body "invoice" --since 1m`
**Search headers and body**: `slashmail search --text "quarterly report"`
**Search all accounts**: `slashmail search --all-accounts --text "quarterly report"`
**Read a message**: `slashmail read --from "boss@example.com" --limit 1`
**Draft a new message**: `printf '%s\n' 'Draft body' | slashmail draft --to recipient@example.com --subject "Subject"`
**Draft a reply**: `printf '%s\n' 'Reply body' | slashmail reply --folder INBOX 1842`
**List received attachments**: `slashmail attachments --folder INBOX 1842`
**Clean up old newsletters**: `slashmail delete --from "newsletter@" --before 3m --dry-run` then confirm with user before running without `--dry-run`
