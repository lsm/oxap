# Decision 0070: One Server with Agent, Provider, and Hub Services

Status: proposed for OXAP 0.2.0
Date: 2026-11-20
Profiles: Agent Control and Model Provider; a separate messaging protocol is proposed
Amends: [the CLI draft](../drafts/cli.md), the [multi-session binding](../drafts/agent-service.md),
the [raw stdio binding](../drafts/endpoint-stdio.md), the CLI form in
[Decision 0027](0027-composed-stdio-profiles.md), the daemon naming and command
in [Decision 0038](0038-one-released-binary-and-a-library-for-every-language.md),
and the compatibility policy in [Decision 0067](0067-the-project-is-named-oxap.md)
for the 0.2 release

## Context

OXAP currently serves Agent Control through a multi-session daemon and a raw
stdio endpoint. Both use the same harness adapter contract. For example, both
construct the same Codex adapter type. The daemon keeps a registry and lets
clients select an adapter at session open. The raw endpoint selects one adapter
at startup. The daemon's old `hub` name now overlaps with a distinct need:
agents and integrations need a common way to exchange messages.

## Decisions

### A configuration file selects the services

`oxap serve --config server.yaml` is the server entry point. `serve` is the
action. A present service section enables that service. An absent section
leaves it disabled. OXAP 0.2 accepts `agent` and `provider` alone or together.
The `hub` section is reserved and rejected until the messaging service ships.
Each service chooses one transport with either `http: <address>` or `stdio: <mode>`.
The file accepts a strict YAML subset and JSON for generated configurations.
The server rejects duplicate or unknown configuration members, an empty
service set, a service without a binding, and conflicting bindings at startup.
Each HTTP listener requires an explicit address. The multi-session Agent
listener keeps its single-user, loopback-only deployment contract.

| Section | Responsibility | Protocol |
| --- | --- | --- |
| `agent` | Open, resume, and drive sessions through configured adapters | Agent Control |
| `provider` | Describe models and serve inference | Model Provider |
| `hub` | Register agent instances and route messages and events | Agent messaging, to be specified |

### Agent Control owns both agent bindings

`agent.adapters` is the registry for the Agent service. It can include OXAP's
own loop and adapters for external harnesses such as Codex and Claude Code.
The [multi-session binding](../drafts/agent-service.md) uses that registry to resolve the
adapter selected at session open. The [raw stdio binding](../drafts/endpoint-stdio.md)
takes an adapter name from its binding configuration and carries raw Agent
Control envelopes. Both resolve adapters from `agent.adapters`. A raw binding
that names a missing adapter fails at startup. The binding determines when the
adapter is selected and how requests are framed.

Selecting raw stdio in both `agent` and `provider` composes the two profiles
on the process's single stdin and stdout. A single raw stdio selection serves
only its profile. The multi-session stdio wrapper requires exclusive use of
the stream. [Decision 0027](0027-composed-stdio-profiles.md) governs the
composition, including routing by profile regardless of configuration order.
Using the co-hosted Provider requires explicit selection. Provider credentials
remain scoped to the connection and profile that received the grant.

### Hub owns messaging

An agent connects to the Hub and registers a routable instance identity. Agent
kind, such as Codex or Claude Code, describes the instance. Direct messages
address a specific instance. Publications address topics, and the Hub delivers
them to matching subscribers. An integration can publish a PR comment event
through the same topic mechanism. Hub runs independently or beside the other
services. An agent or integration registers through the Hub protocol.
Co-hosting creates no registration or binding between an Agent session and a
Hub instance.

The first deployment target is local and self-hosted. The recipient decides
whether a delivery is a notification or should schedule work. Agent Control
continues to govern the lifecycle of any run. The Hub protocol draft must
define the wire, authorization, delivery guarantees, and reconnect behavior
before Hub ships. The Agent and Provider cutover can ship independently in
0.2. Internet hosting is a later deployment target.

### The 0.2 cutover uses one set of names

- Replace the old `hub` command and positional `serve` forms with the
  configuration-driven `serve` entry point. The 0.2 cutover ends the `oapx`
  binary alias window from Decision 0067. SDK launchers use `oxap` and give
  the spawned server a config file, leaving stdin available for the protocol.
- Use Agent Service in product documentation for the existing multi-session
  daemon and rename its draft to `drafts/agent-service.md`. Update the affected
  public SDK types, examples, and test harnesses together. Keep the published
  OXAP package names.
- Use `OXAP_*` for public environment settings, including `OXAP_SDK_BINARY_*`,
  and `oxap` for the built-in adapter name. Remove the old spellings and
  data-directory fallbacks. When only legacy state exists, startup reports
  that it needs migration before creating new state. Keep the existing OS
  credential-store identifier and leave stored credentials in place.
- Set the protocol identity to `oxap` and the wire version to `0.2`. Replace
  the `open-agent-protocol.` prefix of each profile with `oxap.`, giving
  `oxap.agent-control-core` and `oxap.model-provider-core`. For Agent Service
  transport events, replace the `oap-` prefix with `oxap-` and retain each
  event's suffix. Validate the protocol and version before profile routing.
  A recoverable request on the retired wire receives a correlated
  `unsupported_version` error. Agent Control gains this error code in 0.2.
  The 0.2 binding specs define the error's form and framing for raw stdio,
  multi-session stdio, and HTTP. Existing framing error rules apply when a
  request cannot be correlated. Preserve capability revision pins, native
  harness identifiers, and release artifacts.

### Retired-wire errors use the current envelope

The retired wire is the pair `protocol: "open-agent-protocol"` and
`version: "0.1"`. A 0.2 server checks that pair before profile routing or
profile-specific payload decoding. Its refusal is a 0.2 message: wherever the
binding returns an envelope, that envelope carries `protocol: "oxap"`,
`version: "0.2"`, and a fresh response id. It does not echo the retired
protocol identity or version.

Profile validity does not decide whether a retired request is correlatable. On
raw stdio and HTTP, a non-empty string request `id` is sufficient even when the
`profile` is absent, unknown, or wrong; the response's `in_reply_to` repeats
that id exactly. A request without such an id follows the binding's existing
framing or decode-error rules. Multi-session stdio instead correlates on its
valid outer transport id, so the nested envelope does not need an id. The
response profile and exact wire shape are defined by the [raw stdio binding](../drafts/endpoint-stdio.md#retired-01-requests-in-oxap-02),
the [Agent Service binding](../drafts/agent-service.md#retired-01-requests-in-oxap-02),
and the [provider HTTP binding](../drafts/provider-http.md#retired-01-requests-in-oxap-02).

## Consequences

- Decision 0038's multi-session daemon becomes the Agent Service, reachable
  through the `agent` section under `oxap serve --config`. The single released
  binary continues to carry Agent and Provider as separate protocols.
- Update the CLI draft, bindings, SDK launchers, and migration guide in the
  implementation PRs. This decision records the target architecture and
  supersedes the older naming policy when the 0.2 cutover ships.
- Update the shared conformance suites and their Zig, Go, and Rust runners.
  Verify that the packaged build serves a multi-adapter Agent service and
  composes raw Agent and Provider on stdio. Publish the 0.2.0 release after
  those paths work. Hub has its own release gate, based on its protocol draft.
