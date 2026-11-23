#!/usr/bin/env bash
set -u

state=$(mktemp -d) || exit
trap 'rm -rf -- "$state"' EXIT

cargo build --locked --manifest-path rust/Cargo.toml -p oxap-adapter-hermes --example gateway_session
status=$?
printf 'BUILD EXIT: %s\n' "$status"
if ((status != 0)); then
  exit "$status"
fi

example=${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session
output=$("$example" "${OAP_HERMES_BIN:?}" "${OAP_HERMES_ROOT:?}" "$state" --start 2>&1)
status=$?
printf '%s\nGATEWAY EXAMPLE EXIT: %s\n' "$output" "$status"

starts=$(awk '/^prompt started / { count++ } END { print count + 0 }' <<< "$output")
refusals=$(awk '/settlement before the turn opened/ { count++ } END { print count + 0 }' <<< "$output")
clean_exits=$(awk '/^gateway exited cleanly$/ { count++ } END { print count + 0 }' <<< "$output")
printf 'STARTS: %s\nPRE-START SETTLEMENTS: %s\nCLEAN SHUTDOWNS: %s\n' "$starts" "$refusals" "$clean_exits"

[[ "$status" == 1 && "$starts" == 0 && "$refusals" == 1 && "$clean_exits" == 1 ]]
