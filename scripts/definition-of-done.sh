#!/usr/bin/env bash
# The repository's executable definition-of-done gate.
#
# The fast lane is the quality/build gate used by the build circuit. The slow
# lane runs the complete test split documented in AGENTS.md. Resolve the root
# from this file rather than git so the gate also works from a git-archive
# extraction, which intentionally has no .git directory.

set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage: scripts/definition-of-done.sh [--fast|--slow|--all]

  --fast  run formatting, lint, and all-target compilation (default)
  --slow  run the complete test suite (integration tests and doctests)
  --all   run both lanes
EOF
}

lane="fast"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --fast)
            lane="fast"
            ;;
        --slow)
            lane="slow"
            ;;
        --all)
            lane="all"
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
    shift
done

repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repo_root"

run_fast() {
    echo "definition-of-done: cargo fmt --all -- --check"
    cargo fmt --all -- --check

    echo "definition-of-done: cargo clippy --all-targets -- -D warnings"
    cargo clippy --all-targets -- -D warnings

    echo "definition-of-done: cargo build --all-targets"
    cargo build --all-targets
}

run_slow() {
    echo "definition-of-done: cargo test --tests"
    cargo test --tests

    echo "definition-of-done: cargo test --doc"
    cargo test --doc
}

case "$lane" in
    fast)
        run_fast
        ;;
    slow)
        run_slow
        ;;
    all)
        run_fast
        run_slow
        ;;
esac

echo "definition-of-done: PASS ($lane)"
