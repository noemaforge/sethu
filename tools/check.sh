#!/usr/bin/env bash
set -euo pipefail

# Run all gate checks. Fails on the first failing step.

# Ensure ~/.cargo/bin is on PATH so cargo subcommands (nextest) are found.
export PATH="${HOME}/.cargo/bin:${PATH}"

echo "==> Step 1: cargo fmt --check"
cargo fmt --check

echo "==> Step 2: cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> Step 3: tests"
if cargo nextest --version > /dev/null 2>&1; then
    echo "    Using cargo nextest"
    cargo nextest run
else
    echo "    cargo-nextest not found, falling back to cargo test" >&2
    cargo test
fi

echo "==> Step 4: plan-reference scan"
FAIL=0
TRACKED=$(git ls-files 2>/dev/null || true)

for f in $TRACKED; do
    # Three files name the banned shapes for a legitimate reason, so the
    # loop leaves them out. This script holds the shapes as search text.
    # The ignore list holds one as a path rule. The protocol file states
    # the rule itself.
    if [ "$f" = tools/check.sh ] || [ "$f" = .gitignore ] || [ "$f" = AGENTS.md ]; then
        continue
    fi
    # Task ids: T<digits>.<digits> with optional trailing b
    if grep -nP '\bT[0-9]+\.[0-9]+b?\b' "$f"; then
        echo "FAIL: plan reference (task id) in $f" >&2
        FAIL=1
    fi
    # Section marker
    if grep -nF '§' "$f"; then
        echo "FAIL: plan reference (section marker) in $f" >&2
        FAIL=1
    fi
    # Design and diary paths
    if grep -nF 'design.md' "$f"; then
        echo "FAIL: plan reference (design.md) in $f" >&2
        FAIL=1
    fi
    if grep -nF 'dev-diary' "$f"; then
        echo "FAIL: plan reference (dev-diary) in $f" >&2
        FAIL=1
    fi
    # Review rounds
    if grep -nP 'round[0-9]|remediation' "$f"; then
        echo "FAIL: plan reference (review round) in $f" >&2
        FAIL=1
    fi
done

if [ "$FAIL" -eq 1 ]; then
    echo "Step 4 FAILED: plan references found in tracked source" >&2
    exit 1
fi

echo "==> All checks passed"
