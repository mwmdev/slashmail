# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

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

[0.3.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.3.0
[0.2.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.2.0
[0.1.0]: https://github.com/mwmdev/slashmail/releases/tag/v0.1.0
