#!/usr/bin/env bash
set -u
export NODE_ENV=test

go test -count=1 -v ./go/adapter/claude \
  -run '^Test(ConfiguredProviderReachesTheCLIPeer|HeadersReachTheCLIAsItsCustomHeadersVariable|ProviderSetsTheCLIEndpointAndOneCredentialPerAuthMode|ProviderRefusesWhatItCannotCarry)$'
go_status=$?
printf 'Go provider verification exit=%s\n' "$go_status"

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test adapter_open --test session_open -- --nocapture
rust_status=$?
printf 'Rust provider verification exit=%s\n' "$rust_status"

test "$go_status" -eq 0 && test "$rust_status" -eq 0
