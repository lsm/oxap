# GLM catalog setup

Follow the shared setup in `SKILL.md`, including the verification mutex.
Build the Zig binary, set `CARGO_HOME` to an isolated directory, and run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/glm-catalog.py "$EVIDENCE/glm-zig"
NODE_ENV=test python3 .claude/skills/verify/scripts/go-glm-catalog.py "$EVIDENCE/glm-go"
NODE_ENV=test python3 .claude/skills/verify/scripts/rust-glm-catalog.py "$EVIDENCE/glm-rust"
```

Require exit 0 and `PASS` for each. The Zig served session and Go provider
runtime discover a model and complete a turn against a loopback GLM-compatible
endpoint using a synthetic environment key. They check request paths and
bearer authorization. Rust has a catalog consumer but no native built-in loop
(#1863); its check covers the two catalog endpoints, discovery path, wire and
environment credential. These checks do not call a live provider or verify
Claude Code web search.
