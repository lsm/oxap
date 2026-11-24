# Codex app-server 0.171.0 mapping

The `rust-v0.171.0` release in https://github.com/openai/codex resolves to
commit `ccf9be8cc458bd6b8336fe84d68d8893f2691a5e`, tree
`df2295d59618a37eaca4d41ef5efe6740b23e0b1`. The generated app-server schema
contains 1,131 files and has sorted-path SHA-256
`b609a546ecb28431cdccf93934875c58780b70717a43b29f2ffe44dc5b594687` using the
recipe in the preceding ledger. The same recipe reproduced the v0.167.2 digest.
The published macOS arm64 archive is 117221737 bytes with SHA-256
`9414c8c967f110b11e8324c0cfa2b20f012094ef890110a561c92ee535a2681a`; the Linux
x64 musl archive is 122664925 bytes with SHA-256
`01baa2e47809f8597be5b2f8abd4e4dfe3fb5bcb6e504c2fbaaad97056c15e8d`. Both
archives were hashed independently.

The generated schema differs in 129 files from v0.167.2. It adds
`thread/search/partial`, which the adapters do not project, and optional
`agentMessage.moderation`, whose display can describe `show`, `hide` or `block`.
Codex's source describes that display instruction for a recovered answer;
OAP native history cannot represent it. Go and Zig refuse a native history
containing any non-null moderation rather than silently return its text. Go,
Rust and Zig fail an active run if an item completion contains non-null
moderation, because the adapters cannot project its presentation instruction.

The app server can stream text deltas before it sends an item completion. Such
deltas are forwarded as received and cannot be recalled if moderation later
arrives. This adapter therefore does not claim to enforce Codex's moderation
presentation policy on live streamed text. A consumer needing that policy must
not treat the adapter's streamed output as an enforcement boundary. The
recovery and live behavior are a recorded loss of fidelity, not a claim that
OAP implements Codex's presentation contract.

The v0.167.2 corpus remains a regression floor with its original native frames;
its expected revision is the current descriptor. The v0.171.0 corpus is a
synthetic adaptation of those examples against the new schema, including a
constructed moderated item; it is not a trace recorded from v0.171.0. A credential-free Linux process probe of the candidate
merged with main initialized OAP and opened a session with the v0.171.0 binary;
the endpoint exited 0. It did not submit a prompt or exercise moderation.

## Inline images in released Zig builds

Before binding a capability revision, Zig reads the native `model/list`
catalog. The adapter uses the hub model setting, then the effective model from
Codex `config/read`, then the catalog's default. It enables the feature when the
selected model's `inputModalities` includes `image`. The adapter uses the
effective thread or per-turn model when admitting a submission. Absent catalog
evidence leaves image input unavailable. These descriptors describe the
configured route, as with the DeepSeek adapter.

Zig maps each text part to a native text item and each inline image part to
`{type: "image", url: "data:<media_type>;base64,<data>"}` in order. It accepts
PNG, JPEG, GIF and WebP with standard base64. Each image is capped at 4 MiB
decoded, and a submission can carry at most 6 MiB of image base64. The complete
image turn frame is capped at 7 MiB, below the 16 MiB hub envelope budget and
with 1 MiB headroom under the 8 MiB native frame limit for echoed input.
An unsupported model returns `unsupported_feature` before a native turn starts.

To reach parity, the Go and Rust Codex adapters each need to read the paginated
native model catalog and Codex's effective default, bind the route descriptor and its revision to the
advertised modality, and check the thread or per-turn model before admission.
They also need to preserve ordered message parts on `turn/start`, accept the
same inline MIME and standard base64, enforce the same three limits, and run
the synthetic hub journey on their binaries.
