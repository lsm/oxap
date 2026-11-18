# oxap

`cargo add oxap` gives you [`oxap-sdk`](https://crates.io/crates/oxap-sdk), the Rust SDK for the oxap runtime, at the same version: every public item of `oxap-sdk` is re-exported here, so `oxap::Client` is `oxap_sdk::Client`. The `download` feature turns on `oxap-sdk/download`.

The SDK talks to the `oxap` binary, which this crate does not build. Install the binary from the [releases](https://github.com/lsm/oxap/releases) or with `npm install -g @oxap/cli`.
