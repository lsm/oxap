#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test session_open claude_child_settlement_ -- --nocapture
status=$?
printf 'Claude background settlement verification exit=%s\n' "$status"
exit "$status"
