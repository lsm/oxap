#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test native_read -- --nocapture
status=$?
printf 'Claude native transcript verification exit=%s\n' "$status"
exit "$status"
