#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --lib provided_lifecycle -- --nocapture
status=$?
printf 'Claude provided tool lifecycle verification exit=%s\n' "$status"
exit "$status"
