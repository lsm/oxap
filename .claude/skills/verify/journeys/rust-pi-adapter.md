# Rust Pi adapter

Follow shared setup and the verification mutex in SKILL.md. Run from the
repository root.

```sh
(cd rust && cargo test -p oxap-adapter-pi --test adapter_open)
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
