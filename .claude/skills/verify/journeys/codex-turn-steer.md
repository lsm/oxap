# Codex turn steering

Build the Zig hub using the shared Verify setup. Run the journey with the
pinned Codex CLI (see `harnesses/codex-app-server.json`).

```sh
python3 .claude/skills/verify/scripts/codex-turn-steer.py \
  "$OXAP_BIN" "$(command -v codex)" "$EVIDENCE/codex-turn-steer"
```

The script starts a disposable stdio hub and real Codex app server with an
empty home and a loopback Responses fixture on a free port. The fixture holds
the first model response until the steer is admitted. While guidance is
pending, the state snapshot must list its submit request in
`admitted_submit_requests`. Expect one native
`turn/start`, one native `turn/steer`, a correlated applied event before the
original run's terminal, and the guidance in the next model request. Both a
wrong target and a completed target must be refused. A steer with run controls
must be refused for the specific control, with an unadvertised control taking
precedence over an advertised one. The script retains the native requests,
OAP events and hub stderr.

Run the raw stdio agent endpoint against the scripted Codex app server to
test the case where a native delta arrives before the steer acknowledgment.

```sh
python3 .claude/skills/verify/scripts/codex-steer-order.py \
  "$OXAP_BIN" "$EVIDENCE/codex-steer-order"
```

Expect the steer response to cite sequence 1, followed by the delta at 2,
the applied event at 3, and the terminal at 4. The script retains the raw
trace and requires the Zig validator to accept it with exit 0.
