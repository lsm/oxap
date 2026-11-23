# DeepSeek model provider routing

Follow the shared setup and verification mutex in SKILL.md. Build and run from
the repository root:

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/deepseek-provider-route.py --binary "$EVIDENCE/oxap/bin/oxap"
```

The probe opens a DeepSeek session through `oxap serve --stdio` with
`model_provider: "opencode-go"` and a local fixture in place of dsh. The fixture
checks the child receives the catalog endpoint, the translated credential, and
the entry's own environment. The hub must exit 0 after one dsh initialize for
`deepseek-official` and `deepseek-v4-pro`. Five invalid configurations must each
fail at startup and name the entry.

For live verification, use the catalog-pinned dsh and an existing
`OPENCODE_API_KEY` in the hub process environment. Use a fresh `DSH_HOME`
without managed credentials. Serve the routed entry, submit one turn on
`deepseek-v4-pro`, and observe streamed output followed by `run.completed`.
This is the check for OpenCode Go entitlement and dsh request compatibility.
Keep credential values out of the config and evidence.
