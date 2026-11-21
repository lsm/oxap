# Design: route hub entries through a custom model endpoint

Status: design, not implemented. Written 2026-11-21.

## Problem

A routed hub entry (`model_provider`, merged in #1944) can name only a row of
`providers/catalog.json`. A user whose endpoint is not catalogued — a corporate
gateway, a LiteLLM-style proxy, a local OpenAI- or Anthropic-compatible server —
has no way to point a routed entry at it, and the catalog is not the place for
one user's gateway. The design below adds the smallest surface for it: a base
URL plus the *name* of an environment variable holding the key, by reference,
never the key itself.

## What each harness natively supports

Whether a hub can translate a custom endpoint at all is settled by what the
harness itself reads. Checked against this repo's ledgers and the pinned
upstream documentation:

- **Claude Code** (`claude`): `ANTHROPIC_BASE_URL` for the endpoint, with
  `ANTHROPIC_AUTH_TOKEN` (`Authorization: Bearer`) or `ANTHROPIC_API_KEY`
  (`X-Api-Key`) for the key — environment variables read at process start; which
  credential variable an endpoint wants is the vendor's choice (2.1.282 ledger,
  *Model-provider settings*). Only the `anthropic-messages` wire is reachable.
- **Codex** (`codex`): its own `-c` settings — `model_providers.<id>.base_url`
  and `.env_key`, with `wire_api` fixed to `responses`; the key is read from the
  named variable by codex itself (0.160.1 ledger, *Model-provider settings*).
- **dsh** (`deepseek`): `DEEPSEEK_BASE_URL` and `DEEPSEEK_API_KEY`, resolved per
  request (verified in the dsh-v0.2.2-rc.5 sources; the merged design
  `docs/design/deepseek-opencode-endpoint.md` records the details). The wire is
  Anthropic Messages.
- **OpenCode** (`opencode`): a custom endpoint lives in OpenCode's own config
  file (`providers.<id>.settings.baseURL`, with the key named by the provider's
  `env` list or `{env:NAME}` substitution), discovered from the working
  directory upward; no environment variable names a base URL, and the hub spawns
  `opencode serve` with a fixed argv. There is nothing a hub entry could
  translate into.
- **Pi** (`pi`): custom endpoints live in `models.json` under the agent
  directory (`providers.<id>.baseUrl`, `apiKey` interpolating `$NAME`); the only
  base-URL variable is Azure-specific (`AZURE_OPENAI_BASE_URL`). Not
  translatable without writing Pi's file.
- **Hermes** (`hermes`): the gateway is configured by its own args and
  environment, which the entry already passes through verbatim; no single
  documented endpoint variable exists to translate.
- **ACP** (`acp`): the protocol carries an agent the entry names; that agent
  owns its endpoint, and routing it is not the hub's business.
- **Copilot** serves a fixed GitHub API on a Go-only adapter; `memory` does no
  model IO.

So the addition covers exactly the three kinds whose custom-endpoint surface is
a set of environment variables or `-c` settings the hub can append:
`claude`, `codex`, and `deepseek`. For OpenCode and Pi a custom endpoint already
works today through each harness's own config file placed where the harness
reads it (the entry's `working_directory`), with the key referenced as `$NAME`;
no `oxap` change is needed or proposed there.

## Design

One new optional object member, `model_endpoint`, on `claude`, `codex`, and
`deepseek` entries, alongside `model_provider`:

```json
"model_endpoint": {
  "base_url": "https://gateway.example.internal/anthropic",
  "credential_env": "GATEWAY_API_KEY",
  "auth": "bearer"
}
```

`base_url` and `credential_env` are required; `auth` is optional, admitted only
on a `claude` entry, and takes `"api-key"` (the default) or `"bearer"`. The
member stays `oapx`'s own, refused by Go and Rust as unknown, exactly as
`model_provider` is.

`oapx` treats the object as a synthesized catalog row and feeds the routing
paths #1944 already runs: `routedClaudeEnvironment` / `routedCodex` /
`routedDeepseekEnvironment` take their row from the object instead of a catalog
lookup, and everything downstream — appended after the entry's own
`environment`, refusal of an entry that already sets a translated variable — is
unchanged. Per kind:

- `claude`: `ANTHROPIC_BASE_URL=<base_url>` and, by `auth`,
  `ANTHROPIC_API_KEY=<key>` or `ANTHROPIC_AUTH_TOKEN=<key>`.
- `codex`: the same `-c` settings a catalog row produces, under the fixed
  provider id `oapx-custom` — `model_providers.oapx-custom.base_url=<base_url>`,
  `env_key="OAPX_PROVIDER_API_KEY"`, `wire_api="responses"` — with
  `OAPX_PROVIDER_API_KEY=<key>` appended.
- `deepseek`: `DEEPSEEK_BASE_URL=<base_url>` and `DEEPSEEK_API_KEY=<key>`.

The key resolves from one place: the hub process environment, under the name
`credential_env` gives. No credential-store fallback exists for a custom
endpoint — the store is keyed by catalog provider id, and a custom endpoint has
none — and no OAuth login can be involved, so an env reference is both the whole
mechanism and the whole refusal surface. The raw name is never forwarded to the
child: the harness receives only the translated variables, the same property the
catalog routing has.

### Refusals

Each names the entry, at startup:

- `model_endpoint` on any kind but `claude`, `codex`, or `deepseek` — the same
  message shape `model_provider` gets, naming the kinds that translate it.
- `model_provider` and `model_endpoint` together: two providers named.
- `auth` on a `codex` or `deepseek` entry.
- `base_url` that is not an absolute `http://` or `https://` URL with a host;
  `http://` is admitted only on a loopback host, keeping plaintext off the
  network. The URL is passed through verbatim — no trailing-slash or `/v1`
  adjustment — because each harness appends its own paths (dsh appends `/v1`
  unless present, Claude Code its messages path, codex `/responses`), so the
  caller writes the base each harness's convention expects.
- `base_url` equal to an endpoint the catalog already records for the entry's
  wire: the catalog row is the way to name it, and this keeps the catalog the
  one place a provider endpoint is spelled (`provider_catalog` already exposes
  the rows to compare against).
- `credential_env` empty, or unset or empty in the hub environment.
- An entry `environment` that already sets a translated variable for its kind
  (`ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`;
  `OAPX_PROVIDER_API_KEY`; `DEEPSEEK_BASE_URL`, `DEEPSEEK_API_KEY`) — the
  existing `refuseRoutedVariables` list, extended.

Scope: `zig/src/adapter/config.zig` (admit the member and its object's fields on
the three kinds), `zig/src/tools/makai.zig` (the synthesized row and the `auth`
branch), tests beside the routing tests, and a `drafts/hub.md` paragraph in the
same PR. Go and Rust change nothing.

### What HyperNeo changes (its own repo)

A provider row using an OXAP harness gains two optional settings: the endpoint
URL and the credential's environment-variable *name* — a name, never a value;
the value continues to come from the daemon environment, as `OPENCODE_API_KEY`
does today. The row's `entry()` emits `model_endpoint` when both are set, and
the Settings screen shows two fields. `buildHubConfig` changes nothing else:
the daemon's environment must already carry the named variable for the hub
process to resolve it.

## Alternatives rejected

- **Writing the harness's own config file** (OpenCode's `opencode.json`, Pi's
  `models.json`): that is a file the hub would have to author, place, and keep
  in sync with the entry — a second configuration surface and a new service,
  which this design refuses to become. The user can still write it themselves
  today.
- **Letting `model_provider` take an object**: it changes the member's type and
  muddies its contract ("names a catalog row"); a separate member keeps both
  readable and both refusable in one message.
- **Forwarding the user's variable under its own name**: works for no kind —
  Claude Code, codex, and dsh each read different names — and would put a
  key-bearing name into the child environment for nothing.

## Open questions

1. **Claude Code's credential scheme is unknowable from the URL.** The `auth`
   member defaults to `api-key`; whether gateways in the wild skew `bearer`
   enough to flip the default is only answerable once real endpoints are routed.
2. **Is non-loopback `http://` ever legitimate** (an internal LAN gateway on
   plaintext)? The loopback-only rule is the safe default; loosening it is a
   one-line change if someone needs it.
3. **Entitlement and streaming at a custom endpoint** are verifiable only by the
   live run this design unblocks; `oxap` cannot check the wire a URL speaks.
4. **dsh's managed credentials shadowing** carries over from the merged
   OpenCode design: routing reads the key from dsh's environment only while
   `$DSH_HOME/.credentials.yaml` holds none.
