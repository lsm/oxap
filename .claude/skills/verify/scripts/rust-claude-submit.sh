#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test adapter_open --test session_open -- --nocapture
status=$?
printf 'Claude submit verification exit=%s\n' "$status"
exit "$status"
