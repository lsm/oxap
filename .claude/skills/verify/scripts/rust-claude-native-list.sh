#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test native_list -- --nocapture
status=$?
printf 'Claude native session listing verification exit=%s\n' "$status"
if [ "$status" -ne 0 ]; then
  exit "$status"
fi

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-serve \
  --test stdio_work_start -- --nocapture
status=$?
printf 'Rust work.start verification exit=%s\n' "$status"
exit "$status"
