# Rust harness pin literals

From the repository root, with the verify skill's `/tmp/oap-verify.lock` held,
run `python3 .claude/skills/verify/scripts/rust-pin-literals.py`. The script
builds `goap`, writes two Rust literals from the current catalog into a
temporary source file, and requires `goap check` to reject both with the
repository-relative path and line numbers. It removes the probe and requires
`goap check` to pass against the clean source. Expect exit 0 and
`PASS: 2 checks`.
