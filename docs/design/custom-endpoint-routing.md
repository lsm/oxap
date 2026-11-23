# Design: route hub entries through a custom model endpoint

Status: design, not implemented. Written 2026-11-21, revised the same day after
review; the Hermes section was amended 2026-11-21 after re-checking the pinned
v0.23.3 sources corrected how the provider is selected and which wire the
endpoint speaks.

## Problem

A routed hub entry (`model_provider`, merged in #1944) can name only a row of
`providers/catalog.json`. A user whose endpoint is not catalogued — a corporate
gateway, a LiteLLM-style proxy, a local OpenAI- or Anthropic-compatible server —
has no way to point a routed entry at it, and the catalog is not the place for
one user's gateway. The design below adds the smallest surface for it: a base
URL plus the *name* of an environment variable holding the key, by reference,
never the key itself, for every harness that natively supports a custom
endpoint.

## What each harness natively supports

Whether a hub can translate a custom endpoint is settled by what the harness
itself reads. Checked against this repo's ledgers and the pinned upstream
sources and docs:

- **Claude Code** (`claude`): `ANTHROPIC_BASE_URL` for the endpoint, with
  `ANTHROPIC_AUTH_TOKEN` (`Authorization: Bearer`) or `ANTHROPIC_API_KEY`
  (`X-Api-Key`) for the key — read at process start; which credential variable
  an endpoint wants is the vendor's choice (2.1.282 ledger, *Model-provider
  settings*). Only the `anthropic-messages` wire is reachable.
- **Codex** (`codex`): its own `-c` settings — `model_providers.<id>.base_url`
  and `.env_key`, with `wire_api` fixed to `responses`; the key is read from
  the named variable by codex itself (0.160.1 ledger, *Model-provider
  settings*).
- **dsh** (`deepseek`): `DEEPSEEK_BASE_URL` and `DEEPSEEK_API_KEY`, resolved
  per request (verified in the dsh-v0.2.2-rc.5 sources; the merged design
  `docs/design/deepseek-opencode-endpoint.md` records the details). The wire is
  Anthropic Messages.
- **OpenCode** (`opencode`): the server process reads `OPENCODE_CONFIG` (a
  file) and `OPENCODE_CONFIG_CONTENT` (inline config content) as config layers
  beside the discovered ones (server-process.ts at the pinned tree, lines
  132-133). A custom endpoint is a `providers.<id>` entry with
  `settings.baseURL`, a runtime `package` for the wire, and the key named by
  the provider's `env` list or `{env:NAME}` substitution (opencode.ai/docs
  /providers, checked 2026-11-21). Translatable without writing any file.
- **Pi** (`pi`): custom endpoints live in `models.json` under the agent
  directory, whose location `PI_CODING_AGENT_DIR` names; `providers.<id>`
  takes `baseUrl`, an `api` for the wire
  (`openai-completions`, `openai-responses`, `anthropic-messages`,
  `google-generative-ai`), and a `models` list, with `apiKey` interpolating
  `$NAME` or `${NAME}` from the environment (pinned v1.6.0
  configuration.md and models-configuration.md). Translatable by pointing the
  variable at a directory the hub owns and generating that one file.
- **Hermes** (`hermes`): a custom endpoint is the `openai-api` provider with
  `OPENAI_BASE_URL` — which Hermes honors *only* for that provider — and
  `OPENAI_API_KEY`, with the key "sent only to that URL's exact origin
  (scheme, host and port), never to another port or to `http://` on the same
  host" (pinned v0.23.3 environment-variables.md, integrations/providers.md).
  The provider is selected by a separate `session.create` parameter: the
  gateway's create-override path reads `provider` from the params object and
  leaves it `None` when absent, falling back to profile config, then
  `HERMES_INFERENCE_PROVIDER`, then `auto` (pinned v0.23.3
  `tui_gateway/methods_session_model_guard.py` lines 32-40,
  `hermes_cli/runtime_provider.py` `resolve_requested_provider`). The
  `openai-api` overlay's transport is `codex_responses` (pinned v0.23.3
  `hermes_cli/providers.py` lines 28-35), so the wire is OpenAI Responses;
  URL-based detection overrides it only for host-mandated hosts and
  `/anthropic`-suffixed paths (`_fallback_api_mode`). But the env pair and
  the create parameter are not on their own authoritative: a matching
  `model.base_url` in `config.yaml` beats the environment URL
  (`_api_key_provider_runtime`), the credential pool's `openai-api` rung runs
  before the environment rung (`_ladder_rungs`), and a persisted
  `model.api_mode` recorded for the provider overrides the overlay
  (`_configured_or_fallback_api_mode`). All of that state lives under one
  root: `HERMES_HOME` holds `config.yaml`, `.env` and the auth store the
  credential pool reads (pinned v0.23.3 environment-variables.md,
  `hermes_cli/auth.py` `read_credential_pool`). `config.yaml`'s
  `model.base_url` with `custom_providers`/`key_env` is the file-based
  alternative; the env pair plus the create parameter is the surface a hub
  entry can set.
- **Copilot** (`copilot`, Go-only adapter): BYOK through
  `COPILOT_PROVIDER_BASE_URL` and `COPILOT_PROVIDER_API_KEY`, used for model
  requests regardless of GitHub sign-in (GitHub Copilot CLI auth docs, checked
  2026-11-21).
- **ACP** (`acp`): the protocol has no universal endpoint setting; the pinned
  Docker Agent (`cagent`, the `docker-agent` executable the example config
  names) covers custom endpoints in the agent YAML the entry already points
  at: `providers.<id>.base_url`, `api_type`, and `token_key` naming the
  environment variable that holds the key (pinned cagent providers/custom
  docs). Translatable by forwarding that one variable by name.
- **memory** does no model IO.

## Design

One new optional object member, `model_endpoint`, on `claude`, `codex`,
`deepseek`, `opencode`, `pi`, and `hermes` entries, alongside `model_provider`:

```json
"model_endpoint": {
  "base_url": "https://gateway.example.internal/anthropic",
  "credential_env": "GATEWAY_API_KEY",
  "wire": "anthropic-messages",
  "auth": "bearer"
}
```

`base_url` and `credential_env` are required. `wire` is required on `opencode`
and `pi` entries and takes `anthropic-messages`, `openai-completions`, or
`openai-responses`; on the other kinds the harness fixes the wire, and a
`wire` that contradicts it (`claude`/`deepseek` must say `anthropic-messages`,
`codex` and `hermes` `openai-responses`) is refused.
`auth` is optional, admitted only on a `claude` entry, and takes `"api-key"`
(the default) or `"bearer"`. The member stays `oapx`'s own, refused by Go and
Rust as unknown, exactly as `model_provider` is.

The key resolves from one place: the hub process environment, under the name
`credential_env` gives. No credential-store fallback exists for a custom
endpoint — the store is keyed by catalog provider id, and a custom endpoint
has none — and no OAuth login can be involved. No literal key ever appears in
the hub document, in argv, in a generated file, or in a diagnostic; refusal
messages name the variable, never its value. The base URL is not a credential
and travels in the document and, where the harness's own settings are argv
(codex), in argv — which is why the URL itself is constrained below.

Per kind, `oapx` appends after the entry's own `environment`:

- `claude`: `ANTHROPIC_BASE_URL=<base_url>` and, by `auth`,
  `ANTHROPIC_API_KEY=<key>` or `ANTHROPIC_AUTH_TOKEN=<key>`.
- `codex`: the same `-c` settings a catalog row produces, under the fixed
  provider id `oapx-custom` — `model_providers.oapx-custom.base_url=<base_url>`,
  `env_key="OAPX_PROVIDER_API_KEY"`, `wire_api="responses"` — with
  `OAPX_PROVIDER_API_KEY=<key>` appended.
- `deepseek`: `DEEPSEEK_BASE_URL=<base_url>` and `DEEPSEEK_API_KEY=<key>`.
- `opencode`: `OPENCODE_CONFIG_CONTENT=<inline JSON>` defining
  `providers.oapx` with the runtime `package` for `wire`
  (`anthropic-compatible`, `openai-compatible`, or
  `openai-compatible/responses`), `settings.baseURL`, and
  `"apiKey": "{env:<credential_env>}"`, plus a `models` entry for the model
  the entry's `model` member names after the `oapx/` prefix; the variable
  itself (`<credential_env>=<key>`) is appended beside it. The content is a
  config layer that overrides matching settings and leaves every other layer —
  including the user's own global and project config — untouched. Model
  *selection* is separate from definition: the hub launch today leaves the
  adapter's session-create model unset, so a routed entry's `model` member
  (`oapx/<model-id>`, required) is what `oapx` passes at session create —
  the adapter already sends that field when set — and a missing or malformed
  value (no `oapx/` prefix, empty id) is refused at startup.
- `pi`: `PI_CODING_AGENT_DIR=<directory under the oapx state directory>`
  holding one generated `models.json` with `providers.oapx`
  (`baseUrl`, `api` for `wire`, `"apiKey": "${<credential_env>}"`, a `models`
  entry for the model after the `oapx/` prefix), plus the variable appended
  beside it, plus `--provider oapx --model <model-id>` appended after the
  entry's own `args` — the flags Pi documents for deterministic selection —
  with the id taken from the required `oapx/<model-id>` `model` member and
  malformed values refused as for OpenCode. The directory is content-addressed
  — `<state>/pi/<hash of the rendered models.json bytes>` — so any difference
  in what is written, wire included, gets its own directory: two routed pi
  entries with different configs never share a `models.json`, and neither do
  two hub processes running different documents against the same state
  directory, while the same rendered content rewrites byte-identical bytes,
  making any rewrite idempotent.
  The file is written atomically at adapter build (a temp file beside it, then
  rename), so a concurrently starting session never reads a partial file; a
  changed document builds a new adapter and writes before any new session. The
  user's own agent directory is never read or written for this entry — which
  also means settings and extensions it holds do not load for a routed pi
  entry; that is the trade-off of the only deterministic path Pi offers,
  recorded as an open question.
- `hermes`: `OPENAI_BASE_URL=<base_url>` and `OPENAI_API_KEY=<key>`, and
  `HERMES_HOME=<fresh directory under the oapx state directory, created for
  the entry>`, and the adapter passes `provider: "openai-api"` beside the
  entry's `model` at `session.create` — the adapter sends the model there
  today, and the provider rides the same params object — with the required
  `model` member a bare model id, no prefix. The create parameter is what selects
  the provider: present, it wins over profile config and
  `HERMES_INFERENCE_PROVIDER` for that session, and the origin-binding rule
  ties the key to the endpoint. The hub-owned home is what makes the route
  authoritative: with no `config.yaml` there is no `model.base_url` to beat
  the environment URL and no `model.api_mode` to override the overlay, with
  no auth store the pool rung finds nothing and the environment rung
  resolves the pair, and with no `.env` no ambient provider variable
  competes. Nothing oapx writes goes into it — Hermes keeps its own session
  state there, shared at worst between entries, which cannot misroute
  because the route travels in the process environment; the user's own
  Hermes login, settings and extensions do not load for a routed entry, the
  same trade-off as Pi's hub-owned directory. The wire is Responses, the
  overlay's transport, once the URL re-classification refusal below has
  run. One shadow source survives the fresh home: the pool seeds itself
  from the *environment* — for every registry key variable it also reads
  the numbered siblings `VAR_2`, `VAR_3`, … (`_seed_from_env`,
  `_env_key_var_candidates`), and each seeded `openai-api` row carries the
  environment's `OPENAI_BASE_URL`, so a numbered `OPENAI_API_KEY_2` sibling
  in the child environment adds a second row the pool's rotating selection
  can pick — another account's key presented to the routed endpoint. The
  hub's environment allowlist keeps an unlisted ambient sibling out of the
  child, so the one path in is the entry's own `environment` list, and
  listing a numbered sibling is refused below. The sweep of the other
  environment-seeded rungs comes back clean for this route: other
  providers' pools seed from their own variables but their ladder rungs
  never run for an explicit `openai-api` selection, the OpenRouter
  fallback rung is unreachable because an explicitly selected provider
  resolves or raises rather than falling through, and `.env` — which
  `get_env_prefer_dotenv` prefers over the process environment — does not
  exist in the hub-owned home. The journey's hermes case runs a stub that
  speaks Responses and asserts the requests it receives are
  Responses-shaped (the responses path, not chat completions) and carry
  the resolved key, beside a decoy home seeded with a conflicting
  `config.yaml` (`provider`, `model.base_url`, `model.api_mode` pointing
  at a decoy stub), a decoy `.env` carrying decoy `OPENAI_*` values, and
  a pooled `openai-api` credential: every request must still arrive at
  the real stub and the decoy must receive none. The redaction assertions
  are the existing journey's.

For Copilot and ACP the surface above is not extended — their adapters are
Go-only and the member stays `oapx`'s — but both are covered by forwarding by
reference through the `environment` allowlist both hubs already have: a
`copilot` entry lists `COPILOT_PROVIDER_BASE_URL` and
`COPILOT_PROVIDER_API_KEY`, and a `docker-agent` ACP entry lists the variable
its agent YAML names in `token_key` (the URL and the `token_key` reference
live in the YAML the entry already points at). No oxap change is needed for
either; what the design owes them is the exact names, recorded above.

### Refusals

Each names the entry, at startup:

- `model_endpoint` on any kind but the six above — the same message shape
  `model_provider` gets, naming the kinds that translate it.
- `model_provider` and `model_endpoint` together: two providers named.
- `auth` on any kind but `claude`; `wire` missing on `opencode`/`pi`, or
  contradicting the kind's fixed wire elsewhere.
- `base_url` that is not an absolute `http://` or `https://` URL with a host,
  or that carries userinfo (`user:pass@host`) or any query or fragment
  component: the URL travels verbatim in the hub document and, for codex, in
  argv, and query strings are the common gateway key-in-URL form — so the URL
  contract itself cannot carry a credential, rather than trying to recognize
  one. `http://` is admitted only on a loopback host, keeping plaintext off
  the network. The URL is passed through verbatim otherwise — no
  trailing-slash or `/v1` adjustment — because each harness appends its own
  paths (dsh appends `/v1` unless present, Claude Code its messages path,
  codex `/responses`), so the caller writes the base each harness's convention
  expects.
- `credential_env` empty, or unset or empty in the hub environment.
- An entry `environment` that already sets a *destination* variable for its
  kind (`ANTHROPIC_BASE_URL`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`;
  `OAPX_PROVIDER_API_KEY`; `DEEPSEEK_BASE_URL`, `DEEPSEEK_API_KEY`;
  `OPENCODE_CONFIG`, `OPENCODE_CONFIG_CONTENT`; `PI_CODING_AGENT_DIR`;
  `OPENAI_BASE_URL`, `OPENAI_API_KEY`; `HERMES_HOME`).
- An entry `environment` that names the *source* variable `credential_env`
  gives. The environment merge preserves explicitly listed names, so without
  this refusal the raw key would reach the child under its original name as
  well as (or instead of) the translated one. Where the child genuinely needs
  the variable (OpenCode's `{env:}` and Pi's `${}` references), the appended
  value provides it; listing it again is refused, not merged.
- A `codex` entry that also names an `endpoint`: a running app-server chose
  its provider when it started, the refusal `model_provider` already gets. An
  `opencode` entry that names `endpoint` gets the same refusal for the same
  reason: that path attaches to a server already running, which cannot apply
  the overlay.
- A `pi` entry whose `args` already pass `--model` or `--provider`: the
  appended selection flags must not be contradicted.
- A `deepseek` entry whose `args` contain `--patch`: a patch overlay can set
  `baseURL` and `apiKeyEnv`, which take precedence over the environment and
  would silently shadow the route (dsh config precedence, verified in the
  pinned sources). Refused rather than inspected.
- A `deepseek` entry whose resolved `DSH_HOME` holds a `.credentials.yaml`:
  managed credentials take precedence over the environment for the key, so
  the route would be silently shadowed. Checked at startup; the control layer
  keeps `DSH_HOME` pointing at a directory it owns with no such file.
- A `hermes` entry whose `base_url` Hermes's URL detection would
  re-classify off Responses: a host Hermes mandates another wire for
  (`api.anthropic.com` speaks Messages, `api.actual.inc` Chat Completions)
  or a path ending `/anthropic` or `/anthropic/v1`, or `api.kimi.com` with
  `/coding` in the path (pinned v0.23.3 `runtime_provider.py`
  `_HOST_MANDATED_API_MODES` and `_detect_api_mode_for_url`). The fallback
  also refuses a hostname starting with `bedrock-runtime.` under
  `amazonaws.com`, which `providers.py` selects as Bedrock Converse. Detection
  is a pure function of the URL and runs even in the hub-owned home, so a
  re-classifying URL would silently speak a wire the entry does not declare;
  refused rather than re-declared. The journey's refusal case asserts the
  message names the entry.
- A `hermes` entry whose `environment` lists a numbered sibling
  `OPENAI_API_KEY_<n>` (`_2` on: Hermes's pool seeder reads those even in a
  fresh home, and each seeded row carries the routed base URL while
  selection rotates, so a sibling would send another account's key to the
  routed endpoint — pinned v0.23.3 `agent/credential_pool.py`
  `_env_key_var_candidates`, `_seed_from_env`). `credential_env` naming a
  numbered sibling stays allowed: the value then arrives only under
  `OPENAI_API_KEY`. The journey's refusal case covers this list entry too.

Scope: `zig/src/adapter/config.zig` (admit the member and its object's fields
on the six kinds), `zig/src/tools/makai.zig` (the per-kind translation and
refusals), tests beside the routing tests, a `drafts/hub.md` paragraph, and a
verify journey. The journey's stub endpoint asserts the selected upstream and
auth scheme, and a stub *child* that echoes a sentinel key value in its
stderr, in an error the adapter surfaces, and in its normal output proves the
redaction: the routing records each resolved key value, and every surface the
hub emits — startup refusals, adapter diagnostics quoting child bytes,
hub-side logs — replaces that value with its variable name before the bytes
leave. Success and error paths are both exercised; a clean run alone
establishes nothing. The boundary is deliberate: a log file the child writes
itself (the OpenCode server's log) is the child's own, outside the hub's
emission, and the journey's assertions cover the hub-exposed surfaces.

### What HyperNeo changes (its own repo)

A provider row using an OXAP harness gains two optional settings: the endpoint
URL and the credential's environment-variable *name* — a name, never a value;
the value continues to come from the daemon environment, as `OPENCODE_API_KEY`
does today. The row's `entry()` emits `model_endpoint` (adding `wire` where
the harness needs it) when both are set, and the Settings screen shows the two
fields. `buildHubConfig` changes nothing else: the daemon's environment must
already carry the named variable for the hub process to resolve it.

## Alternatives rejected

- **Leaving OpenCode and Pi to their config files**: the file the user writes
  sits outside the hub document, invisible to its validation; the design
  instead derives the one overlay OpenCode needs from inline content and the
  one file Pi needs from a directory the hub owns, both generated from the
  entry. Hand-maintained files remain possible for any kind, as today.
- **Letting `model_provider` take an object**: it changes the member's type
  and muddies its contract ("names a catalog row"); a separate member keeps
  both readable and both refusable in one message.
- **Forwarding the user's variable under its own name** for the env-driven
  kinds: works for no kind uniformly — Claude Code, codex, dsh, and Hermes
  each read different names — and without the source-name refusal it would
  put a key-bearing name into the child environment uncontrolled.
- **Selecting Hermes's provider by environment** (`HERMES_INFERENCE_PROVIDER`
  on the child): profile config beats the env, and a local session reads it
  from the client's export, not the daemon's (pinned v0.23.3
  `runtime_provider.py` `resolve_requested_provider`), so a hub-set value is
  not authoritative; the per-session `session.create` parameter is.

## Open questions

1. **Claude Code's credential scheme is unknowable from the URL.** The `auth`
   member defaults to `api-key`; whether gateways in the wild skew `bearer`
   enough to flip the default is only answerable once real endpoints are
   routed.
2. **A routed pi entry runs on the hub-owned agent directory**, so a user's
   pi extensions and settings do not load for it. Whether Pi grows a layered
   models source (or the entry should copy a named user directory) is an
   upstream question; the live run decides whether the limitation matters. A
   routed hermes entry's hub-owned `HERMES_HOME` carries the same trade-off
   for the same reason — it is the only root that also carries the config,
   `.env` and credential pool that would shadow the route.
3. **Entitlement and streaming at a custom endpoint** are verifiable only by
   the live run this design unblocks; `oxap` cannot check the wire a URL
   speaks.
4. **Copilot's Go-side surface**: the names are recorded above and the
   environment allowlist carries them today; whether the Go hub grows its own
   translation (and refusals) under a separate decision is open, since
   `model_endpoint` is deliberately `oapx`-only.
5. **dsh's beta headers and files API** against a custom Anthropic-wire
   endpoint behave as they do against any non-official endpoint; only the
   live run can say which a gateway accepts.
