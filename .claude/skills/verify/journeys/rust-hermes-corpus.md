# Rust Hermes byte corpus

Use the shared verification mutex and isolated setup from SKILL.md. On the
test host or in CI, run from the repository root.

```sh
cargo test --locked --manifest-path rust/Cargo.toml \
  -p oxap-adapter-hermes --test corpus -- --nocapture
```

Require exit 0 and a passing replay for every case in the current Hermes
catalog manifest. The replay drives the public adapter and session through
an isolated stdio gateway. It checks native writes including reverse
replies, OAP run envelopes, admission and close refusals, journal recovery,
stored-session reopen, and gateway EOF.

The process-exit fixture injects a synthetic disconnect message in Go's
in-process client. The stdio replay observes the real pipe EOF and checks
that diagnostic in the otherwise byte-matched terminal envelope. The
malformed-frame case retains its recorded diagnostic.
