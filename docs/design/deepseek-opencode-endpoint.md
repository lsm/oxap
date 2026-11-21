# Design: route the DeepSeek harness adapter through a catalogued model provider

Status: design, not implemented. Written 2026-11-21.

## Problem

A control layer that selects the `deepseek` adapter (`oxap serve`, entry type
`deepseek`) can only reach the official DeepSeek endpoint with
`DEEPSEEK_API_KEY`. HyperNeo is the first such layer that wants the same
harness — dsh, pinned by `harnesses/deepseek-harness.json` — to serve its
model calls from the OpenCode Go endpoint, with that endpoint's key taken from
the environment by reference rather than typed into a settings screen.

## What dsh already does (verified in the pinned sources)

The upstream tag the catalog holds as current (`dsh-v0.2.2-rc.5`; the local
install is `rc.2`, whose relevant sources are byte-identical for everything
below) resolves its endpoint and key per request, not at build:

- `packages/llm/llm-deepseek/src/config.ts` — the base URL is
  `config.baseURL ?? $DEEPSEEK_BASE_URL ?? https://api.deepseek.com/anthropic`,
  read from the launch environment, so a hub that forwards `DEEPSEEK_BASE_URL`
  moves the endpoint with no patch layer.
- `packages/llm/llm-deepseek/src/credential.ts` and
  `packages/credentials/credentials-local/src/index.ts` — the key resolves from
  `$DSH_HOME/.credentials.yaml` first, then the inherited process environment,
  under the name the plugin's `apiKeyEnv` names (`DEEPSEEK_API_KEY` by
  default). A `DSH_HOME` holding no managed credentials document therefore
  takes the key from the environment the hub forwards.
- `packages/llm/llm-deepseek/src/messages-api.ts` — the wire is Anthropic
  Messages: the request goes to `<baseURL>/v1/messages` (the `/v1` appended
  unless the base already ends in it).

**dsh itself needs no change.** The only endpoint behavior it cannot express
is a custom request header, which matters only if OpenCode Go requires one
(open question below).

## Design

Extend the `model_provider` member to `deepseek` entries, reusing the routing
`claude` and `codex` entries already have (`zig/src/tools/makai.zig`,
`routedRow` / `routedCredential` / `routedClaudeEnvironment`). The member
stays `oapx`'s own: Go and Rust keep refusing it as an unknown member, so a
document carrying it remains an `oapx` document, exactly as for claude and
codex today.

For a `deepseek` entry naming `model_provider: "<row>"`, `oapx` translates at
startup, refusing the entry (naming it) on every failure:

- The row must be catalogued and serve an `anthropic-messages` endpoint; the
  translated base URL is that endpoint as the catalog records it. The first
  row this serves is `opencode-go`
  (`https://opencode.ai/inference/go`, which dsh turns into
  `.../inference/go/v1/messages`).
- The credential resolves as `routedCredential` already does: the row's
  `credential_env` names (`OPENCODE_API_KEY` for both OpenCode rows) read from
  the hub's own environment, then the `oxap` credential store; an OAuth login
  is refused. Nothing is typed into a settings screen and no literal key
  appears in the hub document.
- Two variables are appended after the entry's own `environment`:
  `DEEPSEEK_BASE_URL=<catalogued anthropic-messages endpoint>` and
  `DEEPSEEK_API_KEY=<resolved key>`. The names are dsh's: the endpoint
  override it reads and the default `apiKeyEnv` it resolves. An entry whose
  `environment` already sets either name is refused, as the claude routing
  refuses `ANTHROPIC_*`.
- The entry's `provider` member (the dsh-side provider, `deepseek-official`)
  is untouched, as are the adapter's capability rows: routing moves the model
  IO destination below the harness, and changes nothing the OAP wire reports.

Scope: `zig/src/adapter/config.zig` (admit the member on a third type and
reword its refusal), `zig/src/tools/makai.zig`
(`deepseekBackendConfig` gains the routed-environment branch), tests beside
the claude routing tests, and a `drafts/hub.md` paragraph in the same PR. Go
and Rust change nothing.

### What HyperNeo changes (its own repo)

The `oxap-deepseek` harness row gains one optional setting, the `model_provider`
id, and its `environment` list gains `OPENCODE_API_KEY` (forwarded by
reference, only when the daemon's environment carries it). The row's `entry()`
emits `model_provider` when the setting is present, and `buildHubConfig`
skips injecting its stored `row.key` (`DEEPSEEK_API_KEY`) in that case, since
`oapx` refuses an entry that sets the name twice. The Settings screen for
DeepSeek's harness shows one field; the key field stays for the official path.
No key value is stored or forwarded under a name it did not already have.

## Alternatives rejected

- **Rename in the control layer**: HyperNeo forwards its own
  `OPENCODE_API_KEY` value as `DEEPSEEK_API_KEY` and writes the OpenCode URL
  literally into the hub document. Zero `oxap` change, but it writes a
  catalogued base URL outside `providers/catalog.json` (the one place the
  repo records provider endpoints), lies about a variable name, and gives one
  control layer a private switch no other layer can use.
- **New `base_url` / `api_key_env` members on the deepseek entry**: duplicates
  the routing machinery with weaker rules — no credential resolution, no
  catalog check, no OAuth refusal — and still tempts the URL out of the
  catalog.
- **A dsh `--patch` overlay**: the plugin also accepts `baseURL` and
  `apiKeyEnv` as profile config, so a patch file works, but the patch must
  live on disk outside the hub document, invisible to the hub's validation
  and to every other consumer of the entry.

## Open questions

1. **Does OpenCode Go's Anthropic endpoint require `x-opencode-session`?**
   The catalog records the header on all three `opencode-go` wires and `oxap`
   sends it on its own requests; dsh cannot add a custom request header. If
   the endpoint refuses a request without it, this design is blocked until
   dsh grows header support — that is the one case where dsh needs a change.
   Decisive probe, once the key is exported: one request to
   `.../inference/go/v1/messages` with and without the header.
2. **Which model ids does the Go plan serve?** dsh sends the entry's `model`
   (HyperNeo defaults `deepseek-v4-pro`); the OpenCode Go catalog must serve
   that exact id or the entry's model member must change with the routing.
3. **DeepSeek-specific request extensions.** The plugin sends DeepSeek beta
   headers (`MESSAGES_FILES_BETA`, `MESSAGES_TOOL_CHANGES_BETA`) and a files
   API on the same wire; whether OpenCode Go accepts or ignores them is only
   answerable by the live run this design unblocks.
4. **Managed credentials shadowing.** Routing reads the key from dsh's
   environment only while `$DSH_HOME/.credentials.yaml` holds none; the
   control layer must keep pointing `DSH_HOME` at a directory it owns that
   has no such file.
