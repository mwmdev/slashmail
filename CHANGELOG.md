# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.8.1] - 2026-10-02

### Added

- Non-ASCII search terms also work on servers that advertise `LITERAL-` instead of `LITERAL+`, such as Gmail, for terms up to 4096 bytes

### Changed

- Non-ASCII folder names are shown decoded (`[Gmail]/Messages envoyés` instead of `[Gmail]/Messages envoy&AOk-s`), and folder options and config settings accept either form; `--json` output and `export` file names use the server's listed name (so `--folder inbox` exports `INBOX_<uid>.eml`)
- `--limit` fetches headers only for recent matches when they suffice instead of for every match, on servers without SORT, such as Gmail, and in each folder of `--all-folders` on every server (`search --limit 5` on a 7,480-message Gmail INBOX: 22.9 s to 1.6 s)
- On Proton Mail Bridge, `--all-folders` skips the label views (`Labels/...` and `Starred`), so a labelled message is listed and counted once, from its regular folder (on a 150,000-message account, `search --all-folders --limit 5` went from 33.9 s to 3.1 s and the `count --all-folders` total from 305,131 to 151,285)

### Fixed

- `--all-folders` and `status` skip containers that cannot be opened (`\Noselect`, such as Gmail's `[Gmail]`) instead of warning about or listing them
- On Gmail, `--all-folders` lists a message once even when it has several labels (the INBOX copy if there is one), and actions apply to that copy; `count --all-folders` totals count it once
- `--all-folders` also skips folders inside Trash, Spam/Junk, and All Mail (such as `[Gmail]/Trash/Old`)
- When slashmail orders messages itself (servers without SORT, `--all-folders`, `--all-accounts`), a Date header more than a day after a message arrived counts as arrival plus one day, so a wrong or forged future date no longer pins a message to the top or pushes newer messages out of a `--limit`

## [0.8.0] - 2026-09-28

### Added

- `read --uid <UID>` reads one exact message from the selected folder and fails when that UID does not exist
- `read --json` prints full headers, thread identifiers, flags, decoded text body, and attachment part metadata
- `search --json` rows include `message_id`, `in_reply_to`, `references`, and `seen`/`answered`/`flagged` state
- `draft --json` and `reply --json` print the saved-draft receipt, including the draft UID and Message-ID, as JSON

### Changed

- **Breaking:** `move` takes its destination as `--dest <FOLDER>`; `--to` is the recipient filter for every command. `move --to X` previously failed to parse in every release since 0.3.0
- **Breaking:** plaintext IMAP is refused for every non-loopback host (previously a warning). Loopback plaintext, such as ProtonMail Bridge, still works
- **Breaking:** `export` percent-encodes folder names in file names (`Work/Projects` → `Work%2FProjects_1.eml`, `Work_Projects` → `Work%5FProjects_1.eml`; on Windows lowercase letters are encoded too). Existing exports are not renamed
- **Breaking:** minimum supported Rust version is 1.88, required by mailparse 0.17
- **Breaking:** `-f/--folder` and `--all-folders` can no longer be combined; `--folder` was previously ignored silently
- Empty or whitespace/control-only text filters (`--subject`, `--from`, `--to`, `--cc`, `--body`, `--text`) are rejected instead of matching every message
- `--all-folders` also skips mailboxes marked `\Trash` or `\Junk` (such as "Deleted Items" and "Junk Email"); `delete` and `move` never search their destination, and naming the destination as the source folder is an error
- `search`, `read`, `count`, `export`, and `--dry-run` open folders read-only with `EXAMINE`, so they no longer clear `\Recent`
- `export` without `--force` skips an existing file only when it holds the same message (identical bytes or Message-ID); an existing file holding a different message, such as after a mailbox was recreated, is left unchanged and reported as an error once the other messages are exported
- `delete` and `move` require server support for `MOVE` or `UIDPLUS` and fail before changing anything otherwise
- Non-ASCII search terms are sent as UTF-8 literals and require `LITERAL+`; without it the search fails instead of silently matching nothing
- `search --json` returns full From, Subject, and Date values; only the terminal table truncates them
- `status` and `quota` use the IMAP library's typed response parsers and report every quota resource
- Received attachment bytes no longer include the line break that belongs to the MIME boundary

### Fixed

- The COPY fallback for servers without `MOVE` expunges only the moved UIDs instead of every message flagged `\Deleted` in the folder
- Actions on searched UIDs (`delete`, `move`, `mark`, `export`, `read`) fail if the folder's `UIDVALIDITY` changed since the search
- Folder lookup compares names exactly instead of using the folder name as a LIST pattern, so names with spaces, quotes, backslashes, `*`, or `%` work; folder names with control characters are rejected instead of silently altered
- `--all-folders` skips the special-use `\All` mailbox and no longer skips folders merely containing "all mail" (such as "Small mail")
- `status` shows `?` instead of zero counts for malformed responses, and folder names with parentheses no longer confuse the counts
- Message bodies combine every `multipart/mixed` text segment in order, choose one `multipart/alternative` representation, and follow the `multipart/related` root, in `read`, `read --json`, and reply quotes
- Replies keep In-Reply-To and References when the source Message-ID or References headers contain comments; header values may contain tabs
- `attachments --save --part` decodes only the selected parts, so a corrupt unselected attachment no longer blocks saving
- Unsolicited FETCH responses (flag changes by other clients during a search) no longer add unrelated messages to `delete`, `move`, `mark`, or `export`, and no longer blank out a matched message's headers
- `delete`, `move`, and `mark` act on and count only messages that still exist; receipts report messages another client removed, and failures report the messages already moved or updated, including messages that received only some of several requested flag changes
- Saving attachments stops instead of overwriting an earlier part when two planned names resolve to the same file on case- or normalization-insensitive filesystems; truncated names no longer end in a space or dot

### Security

- Headers, bodies, folder names, file paths, and server or parser errors are rendered inert in the terminal, including the final error message; `--json` output and exported bytes are unchanged
- Exported messages use exclusive creation, never follow symlinks, refuse to replace directories or special files, detect destinations that alias another message, and are created owner-only (`0600`) on Unix, like saved attachments
- Deeply nested MIME messages are rejected by mailparse 0.17's recursion limit instead of risking stack exhaustion
- The destination mailbox of the COPY fallback is now quoted and validated
- Attachment names such as `COM¹.txt`, `CONIN$`, `CONOUT$`, and `NUL .txt` are treated as Windows device names
- Invisible formatting characters (zero-width space, soft hyphen, BOM, word joiner, tag characters outside emoji flags) are shown as spaces in terminal fields and dropped from bodies, so look-alike folder and sender names stay distinguishable

## [0.7.0] - 2026-09-10

### Added

- Automatic loading of password variables from `.env` beside the selected `config.toml`, while preserving values already set in the process environment

### Fixed

- `mark` now uses distinct `--set-flagged` and `--clear-flagged` actions so flagged and unflagged search filters can be combined with flag updates

## [0.6.0] - 2026-07-30

### Added

- Repeatable `--attach PATH` support for new-message and reply drafts, with explicit local-file preflight, body-first MIME attachments, Unicode basenames, and unchanged unsent-draft receipts
- `attachments <UID>` command for listing received MIME attachments as a terminal table or JSON and explicitly saving all or selected stable part IDs

### Security

- Received-attachment extraction uses byte-exact transfer decoding, direct-child filename sanitization, batch collision preflight, exclusive file creation, and explicit `--force` replacement without marking the source message as seen

## [0.5.0] - 2026-07-25

### Added

- `draft` command for saving plain-text or HTML new-message drafts with structured To, Cc, Bcc, and subject fields
- `reply` command for saving automatically addressed and threaded reply-all drafts by source folder and UID, with optional `--no-quote`
- Optional top-level and per-account `sender` and `drafts_folder` configuration
- Draft destination discovery through the server-designated selectable `\Drafts` mailbox, with `--drafts-folder` override
- Stable saved-draft receipt containing account, folder, UID, recipients, and subject
- Multiple named accounts through `[[accounts]]`, with `default_account`,
  `--account`, and read-only `--all-accounts` selection

### Security

- Draft bodies are read exclusively from stdin; draft commands require environment-based credentials and never let an interactive password prompt consume piped content
- Draft headers and destination names reject malformed or control-bearing input, and ambiguous post-APPEND outcomes are never retried automatically

## [0.4.0] - 2026-04-01

### Added

- `read` command to display email content in terminal with HTML-to-text conversion
- `--body` and `--text` search flags for message content search (IMAP BODY/TEXT keys)
- `--smaller` filter (complement to `--larger`)
- `--flagged`, `--unflagged`, `--answered`, `--draft` search filters
- `--json` flag for machine-readable output on `search` and `count` commands
- Unit tests for export module (folder name sanitization, filename format)
- Integration tests for export skip/overwrite behavior

### Changed

- `sanitize_folder_name()` extracted as public function in export module

## [0.3.2] - 2026-03-31

### Added

- AI agent skill file (`skills/slashmail/SKILL.md`) for natural language email management via Claude Code and other agents

## [0.3.1] - 2026-03-05

### Fixed

- Export UID collision — multi-folder exports no longer silently overwrite files when UIDs collide across folders (filenames now prefixed with folder name)
- SORT response parsing — no longer false-positives on server responses containing "BAD" or "NO" as substrings (e.g. `OK [BADCHARSET]`)

### Changed

- IMAP capabilities cached at connect time, eliminating a CAPABILITY round-trip per folder/chunk
- `ensure_folder_exists` uses targeted `LIST "" <folder>` instead of `LIST "" *`
- `ensure_folder_exists` deduplicated between search and delete modules

## [0.3.0] - 2026-02-15

### Added

- `--to` filter — search by To address
- `--cc` filter — search by CC address
- `--seen` filter — match only read messages
- `--unseen` filter — match only unread messages (`--seen` and `--unseen` are mutually exclusive)

## [0.2.0] - 2026-02-13

### Added

- Config file support — load connection defaults from `config.toml` (Linux: `~/.config/slashmail/`, macOS: `~/Library/Application Support/slashmail/`, Windows: `%APPDATA%\slashmail\`)
- `--config <PATH>` flag to specify an alternative config file location
- Relative date shorthand for `--since`/`--before` — use `7d`, `2w`, `3m`, `1y` in addition to `YYYY-MM-DD`
- Configurable `trash_folder` and `default_folder` via config file

## [0.1.0] - 2026-02-10

### Added

- IMAP search with server-side filtering (SEARCH/SORT)
- Bulk delete with interactive confirmation and dry-run mode
- `move` command — move matching messages to any folder
- `export` command — save matching messages as `.eml` files
- `mark` command — set/unset read, flagged status on messages
- `count` command — fast message counting without FETCH
- `quota` command — show mailbox quota usage
- `status` command — per-folder message statistics
- Folder listing with message counts
- Multi-folder search across all mailboxes
- TLS support for remote IMAP servers (Gmail, Fastmail, etc.)
- Localhost defaults (127.0.0.1:1143, plain TCP)
- SORT extension (RFC 5256) with SEARCH fallback
- MOVE with COPY+DELETE+EXPUNGE fallback
- Size filtering with K/M suffixes
- Date range filtering (SINCE/BEFORE)
- Subject and From field search
- Result limiting with pre-FETCH truncation when SORT is available
- Shell completions (bash, zsh, fish, PowerShell, elvish)
- Man page generation
- Cross-platform binaries (Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64)

### Security

- IMAP command injection prevention via input sanitization
- TLS 1.2+ enforced for encrypted connections
- Plaintext connection warning for non-loopback hosts
- Passwords securely zeroed from memory after login

[Unreleased]: https://github.com/mwmdev/slashmail/compare/v0.8.1...HEAD
[0.8.1]: https://github.com/mwmdev/slashmail/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/mwmdev/slashmail/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/mwmdev/slashmail/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/mwmdev/slashmail/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/mwmdev/slashmail/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.4.0
[0.3.2]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.2
[0.3.1]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.1
[0.3.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.0
[0.2.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.2.0
[0.1.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.1.0
