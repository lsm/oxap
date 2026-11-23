# Rust SDK CI cache

On a pull request with a changed `rust/Cargo.lock`, inspect the Rust SDK job
in `ci-zig.yml` for the exact PR head:

```sh
gh run list --workflow ci-zig.yml --event pull_request --commit <head-sha>
gh run view <run-id> --json jobs
gh run view <run-id> --job <rust-sdk-job-id> --log
gh pr checks <pr-number> --required
```

The Cargo cache restore can hit an exact key, fall back to an older key, or
miss. The Rust SDK lint, tests, packaging and conformance steps must pass.
If the job is canceled, inspect its check annotations and the step that was
running. The 25-minute job budget covers restoring the Cargo cache as well as
the verification steps.
Expect the Cargo cache save step to be skipped on the pull request and the
required aggregate check to pass. On a main push with a new key, expect the
save step to run after the checks and publish a cache under the restored
primary key. An exact hit skips the save on main as well.
