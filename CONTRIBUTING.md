# Contributing to slashmail

## Building

```bash
cargo build
```

## Testing

```bash
scripts/test.sh                 # fmt, clippy, unit and scripted tests
scripts/test.sh --integration   # also the GreenMail suite (needs Docker)
scripts/test.sh --ci            # every CI job that runs on Linux (needs Docker and cargo-deny)
```

CI runs the same checks; scripts/test.sh must pass before a PR can be merged.

## Submitting a PR

1. Fork the repo and create a feature branch
2. Make your changes
3. Ensure `scripts/test.sh` passes (add `--integration` when you touch IMAP behavior)
4. Open a pull request against `main`

Keep PRs focused on a single change. If you're fixing a bug, include a test that reproduces it.
