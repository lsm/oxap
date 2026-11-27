# Rust Pi adapter

Follow shared setup and the verification mutex in SKILL.md. Run from the
repository root.

```sh
(cd rust && cargo test -p oxap-adapter-pi --test adapter_open)
(cd rust && cargo test -p oxap-adapter-pi --test reopen_binding)
(cd rust && cargo test -p oxap-adapter-pi --all-features)
```

`tests/fixtures/go-descriptor.json` is the capability descriptor Go's
`pi.Adapter.Probe` marshals, so the Rust descriptor is judged by Go's output,
not by a copy of its source. Regenerate it with a throwaway test in
`go/adapter/pi` that prints `json.Marshal(descriptor.Capabilities)` for an
adapter built with an executable and an absolute working directory. The test
substitutes the catalog's endpoint version, so a pin bump alone does not
require a new file. To see it bite, change one feature's reason or level in
`src/adapter.rs`: `a_probe_advertises_the_capabilities_go_advertises` must
fail. Drop `--no-extensions` from the launch arguments: the launch test must
fail.

`tests/reopen_binding.rs` holds cases probed against Go's `readBinding`
with a throwaway test in `go/adapter/pi` that writes a session file and prints
the error each binding returns. To see it bite, read one byte more of an
unterminated header (`HEADER_LIMIT + 1` to `+ 2`, or `<=` to `<` on the
unterminated branch): the 64 KiB test must fail. Decode the header with
`String::from_utf8_lossy`: the replacement-character test must fail. Ignore an
unknown binding member: the refusal test must fail.
