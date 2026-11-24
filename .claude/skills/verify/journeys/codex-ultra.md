# Codex ultra reasoning

Follow the shared Verify setup. Run from the repository root with a fresh
evidence directory on the build host.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/codex-ultra.py "$EVIDENCE/oxap/bin/oxap"
for fixture in valid/session-settings-at-open valid/session-settings-live-update-level-only schema-invalid/session-settings-unknown-level; do
  "$EVIDENCE/oxap/bin/oxap" validate "fixtures/$fixture.json"
  printf '%s exit=%s\n' "$fixture" "$?"
done
```

The stdio hub must accept and report `ultra` at open and on a live update.
The synthetic app-server records `ultra` in the thread start config and in
the first turn after the update. Both operations must still reject
`persistent` with `schema_invalid`. Expect three native calls and a clean hub
exit. Zig CI runs the same journey without authentication or provider calls.

Both valid traces must exit 0. The unknown-level trace must exit nonzero
with `schema_invalid`.
