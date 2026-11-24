#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test session_open claude_arbitration_drains_stdout_before_settling -- --nocapture
status=$?
printf 'Claude result arbitration verification exit=%s\n' "$status"
exit "$status"
