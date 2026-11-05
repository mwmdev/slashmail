#!/usr/bin/env bash
# Local test runner: fmt, clippy, tests; --integration adds GreenMail, --ci mirrors CI.
# Run from anywhere; see --help.
set -euo pipefail
cd "$(dirname "$0")/.."

usage_error() {
  echo "usage: scripts/test.sh [--integration | --ci]" >&2
  exit 2
}

if [ "$#" -gt 1 ]; then usage_error; fi
case "${1:-}" in
  "") mode=fast ;;
  --integration) mode=integration ;;
  --ci) mode=ci ;;
  -h | --help)
    cat <<'EOF'
usage: scripts/test.sh [--integration | --ci]
  --integration  also run the GreenMail suite (needs Docker)
  --ci           every CI job except windows: check, package, cargo deny, GreenMail, MSRV 1.88 (needs Docker, cargo-deny)
EOF
    exit 0
    ;;
  *) usage_error ;;
esac

if [ -z "${IN_NIX_SHELL:-}" ] && command -v nix-shell >/dev/null; then
  exec nix-shell --run "$(printf '%q ' scripts/test.sh "$@")"
fi

# An sccache server started from an earlier nix-shell keeps that shell's deleted TMPDIR and fails every compile.
export RUSTC_WRAPPER=

if [ "$mode" != fast ] && ! docker info >/dev/null 2>&1; then
  echo "Docker is required for --integration and --ci (is the daemon running?)" >&2
  exit 1
fi
if [ "$mode" = ci ] && ! command -v cargo-deny >/dev/null; then
  echo "cargo-deny is required for --ci: cargo install cargo-deny --locked" >&2
  exit 1
fi

step() {
  printf '\n==> %s\n' "$*"
  "$@"
}

wait_for_greeting() {
  local port=$1 prefix=$2 line=""
  for _ in $(seq 60); do
    line=$(timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port && head -n1 <&3" 2>/dev/null || true)
    if [[ $line == "$prefix"* ]]; then return 0; fi
    sleep 1
  done
  echo "GreenMail did not answer on port $port within 60 s" >&2
  docker logs --tail 20 "$name" >&2
  return 1
}

run_greenmail_suite() {
  name="slashmail-test-greenmail-$$"
  docker run -d --rm --name "$name" -p 127.0.0.1::3143 -p 127.0.0.1::3025 greenmail/standalone:2.1.8 >/dev/null
  # shellcheck disable=SC2064
  trap "docker stop $name >/dev/null 2>&1 || true" EXIT
  local imap smtp
  imap=$(docker port "$name" 3143/tcp | head -n1)
  imap=${imap##*:}
  smtp=$(docker port "$name" 3025/tcp | head -n1)
  smtp=${smtp##*:}
  wait_for_greeting "$imap" '* OK'
  wait_for_greeting "$smtp" 220
  step env GREENMAIL_IMAP_PORT="$imap" GREENMAIL_SMTP_PORT="$smtp" \
    cargo test --features integration-tests --test integration
}

step cargo fmt --all -- --check
step cargo clippy --all-targets --all-features -- -D warnings
step cargo test

if [ "$mode" = ci ]; then
  step cargo package --locked --features vendored-openssl --allow-dirty
  step cargo deny check
fi

if [ "$mode" != fast ]; then
  run_greenmail_suite
fi

if [ "$mode" = ci ]; then
  step docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp -e CARGO_HOME=/tmp/cargo \
    -e CARGO_TARGET_DIR=/tmp/t -v "$PWD":/src -w /src rust:1.88 \
    sh -c 'cp -r /usr/local/cargo /tmp/cargo && cargo check -q && cargo test -q'
fi

printf '\n==> all checks passed (%s)\n' "$mode"
