#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --lib provided_resolution -- --nocapture
status=$?
printf 'Claude provided tool resolution verification exit=%s\n' "$status"
exit "$status"
