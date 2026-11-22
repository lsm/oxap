# DeepSeek batched replies over stdio

Follow the shared isolation and verification mutex in SKILL.md. Build the
shipping Zig runtime and drive it with a synthetic dsh SDK process.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/deepseek-batched-stream.py \
  --binary "$EVIDENCE/oxap/bin/oxap" --wire "$EVIDENCE/deepseek-batched-stream.jsonl"
```

The peer sends one assistant message with 624 reasoning fragments followed by
reply text and a completed turn. The proof subscribes before sending the first
prompt. It compares the complete reasoning and reply text and requires
`run.completed` on the same subscription. The raw wire shows any overflow and
the last delivered sequence. No live provider is used.
