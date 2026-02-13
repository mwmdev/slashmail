# Contributing to slashmail

## Building

```bash
cargo build
```

## Testing

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --all -- --check
```

All three checks run in CI and must pass before a PR can be merged.

## Submitting a PR

1. Fork the repo and create a feature branch
2. Make your changes
3. Ensure `cargo test`, `cargo clippy`, and `cargo fmt --check` pass
4. Open a pull request against `main`

Keep PRs focused on a single change. If you're fixing a bug, include a test that reproduces it.
