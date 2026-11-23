# Rust Claude provider routing

Follow the shared setup and verification mutex in `SKILL.md`. From the
repository root on a test host or in CI, run:

```sh
bash .claude/skills/verify/scripts/claude-provider-launch.sh
```

Require both exit codes to be zero and both `PASS Go provider launch` and
`PASS Rust provider launch`. Each public adapter opens the same local peer
using a temporary home. The peer checks the configured URL, the CLI's
unauthenticated sentinel, the absence of other credentials, and the order
and exact formatting of the custom headers. It completes the corpus
initialize exchange and records stdin EOF. It does not make network requests.

Check the Rust auth plans for native passthrough and all three configured auth
modes. The validation cases refuse unusable URLs, secrets and headers before
the adapter opens a session. Keyed plans stay in memory and never start a peer.
