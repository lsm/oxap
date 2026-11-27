#!/usr/bin/env bash
set -euo pipefail
cargo test --locked --manifest-path rust/Cargo.toml \
  -p oxap-adapter-claude --test catalog_replay -- --nocapture
