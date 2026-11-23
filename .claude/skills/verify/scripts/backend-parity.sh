#!/usr/bin/env bash
set -euo pipefail

: "${OAP_OAPX_BIN:?set OAP_OAPX_BIN to the built Zig endpoint}"
: "${OAP_RUST_HUB_BIN:?set OAP_RUST_HUB_BIN to the built Rust endpoint}"

log=$(mktemp)
trap 'rm -- "$log"' EXIT
go test ./go/cmd/goap -run '^TestBackendsMatchOapx$' -count=1 -v | tee "$log"

fixtures=(acp codex deepseek deepseek-switch memory)
compared=0
for fixture in "${fixtures[@]}"; do
    if ! grep -Eq "^ +--- PASS: TestBackendsMatchOapx/$fixture/rust " "$log"; then
        printf 'Missing Rust comparison for %s\n' "$fixture" >&2
        continue
    fi
    compared=$((compared + 1))
done
printf 'Three-way fixture passes: %d/%d\n' "$compared" "${#fixtures[@]}"
test "$compared" -eq "${#fixtures[@]}"
