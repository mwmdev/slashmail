# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/mwmdev/slashmail/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/mwmdev/slashmail/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/mwmdev/slashmail/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.4.0
[0.3.2]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.2
[0.3.1]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.1
[0.3.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.0
[0.2.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.2.0
[0.1.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.1.0
