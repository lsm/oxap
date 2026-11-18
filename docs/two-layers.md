# Two ways oxap runs a model

`oxap serve agent` (and everything built on it: the TUI, the SDKs,
`oxap run`) answers every client with the same OAP envelopes. Behind it, a
model is reached one of two ways, and the choice is `--backend`.

| | Provider layer | Harness layer |
| --- | --- | --- |
| What runs the agent | oxap's built-in loop | a vendor's own harness, at a pinned version |
| Selected by | no `--backend`, or `--backend oapx` | `--backend claude`, `codex`, `pi`, `opencode`, `acp`, `hermes` or `deepseek` |
| Model chosen by | `--model <provider>/<api>@<model>` | the harness's own configuration, or the entry's `model` |
| Endpoints come from | [`providers/catalog.json`](../providers/catalog.json), and custom providers in `~/.oxap/providers.json` | the harness, or a catalog provider the entry routes it to |
| Configured in | the provider catalog and [custom endpoints](custom-endpoints.md) | a registry entry shaped like [`examples/oap-serve.json`](../examples/oap-serve.json) |
| Pins and gaps | none: the loop is oxap's own | [`harnesses/*.json`](../harnesses), and each harness's ledger in `research/` |

## Provider layer

The built-in loop calls a model API directly, over whichever wire the catalog
row names; it speaks every wire the catalog uses. Any catalogued provider
works, and so does any OpenAI- or Anthropic-compatible
endpoint declared as a custom provider, with extra request headers if it needs
them. Credentials come from `oxap auth login` or the variable the catalog row
lists. The loop owns tools, permissions and compaction itself, whichever model
is behind it.

```sh
oxap run --model <provider>/<api>@<model> "say hi"
```

`OXAP_PROVIDER_SERVICE_URL` moves the provider calls to a remote
`oxap serve provider`, without changing anything above it.

## Harness layer

An adapter drives the vendor's harness and translates it to OAP. Pick this when
the model works better, or costs less, inside its own harness: its prompts,
tools and context handling come with it. Each adapter is pinned to the
versions its catalog entry names, and its ledger records what the harness
cannot express in OAP. [Choosing a backend](choosing-a-backend.md) lists every
backend, what it needs and how it authenticates.

```sh
oxap run --backend codex "say hi"
```

The layers meet in one place: a `claude` or `codex` entry may set
`model_provider` to a catalog provider, and oxap points the harness at that
provider's endpoint with its key. The harness then runs a model the provider
layer would otherwise serve.

## MCP tools

- **Built-in loop:** none yet; stdio and HTTP sources with header, token and
  OAuth auth are #1848.
- **`acp`:** takes the session's `tool_sources` at open and hands them to the
  agent, stdio sources only at the current pin.
- **The other harnesses:** take no tool sources from OAP.
