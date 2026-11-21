# Decision 0070: One Server with Agent, Provider, and Hub Services

Status: proposed for OXAP 0.2.0
Date: 2026-11-20
Profiles: Agent Control and Model Provider; a separate messaging protocol is proposed
Amends: the server command in [the CLI draft](../drafts/cli.md), the CLI form in
[Decision 0027](0027-composed-stdio-profiles.md), and the compatibility policy in
[Decision 0067](0067-the-project-is-named-oxap.md) for the 0.2 release

## Context

OXAP currently serves Agent Control through a multi-session daemon and a raw
stdio endpoint. Both use the same harness adapter contract. For example, both
construct the same Codex adapter type. The daemon keeps a registry and lets
clients select an adapter at session open. The raw endpoint selects one adapter
at startup. The daemon's old `hub` name now overlaps with a distinct need:
agents and integrations need a common way to exchange messages.

## Decisions

### A configuration file selects the services

`oxap serve --config server.json` is the server entry point. `serve` is the
action. The top-level sections `agent`, `provider`, and `hub` select services.
A present section enables its service. An absent section leaves it disabled.
One process can serve any combination that has a valid binding configuration.
An empty service set or conflicting listeners fail before serving. Each service
owns its bindings and settings in its section.

| Section | Responsibility | Protocol |
| --- | --- | --- |
| `agent` | Open, resume, and drive sessions through configured adapters | Agent Control |
| `provider` | Describe models and serve inference | Model Provider |
| `hub` | Register agent instances and route messages and events | Agent messaging, to be specified |

### Agent Control owns both agent bindings

`agent.adapters` is the registry for the Agent service. It can include OXAP's
own loop and adapters for external harnesses such as Codex and Claude Code.
The [multi-session binding](../drafts/hub.md) uses that registry to resolve the
adapter selected at session open. The [raw stdio binding](../drafts/endpoint-stdio.md)
takes an adapter name from its binding configuration and carries raw Agent
Control envelopes. Both resolve adapters from `agent.adapters`. The binding
determines when the adapter is selected and how requests are framed.

Agent and Provider can share raw stdio when the configuration explicitly
composes those profiles. [Decision 0027](0027-composed-stdio-profiles.md) governs
profile routing and output framing. Each stdio stream has one framing owner.
The multi-session wrapper and Hub messaging each require their own binding.
Deploying Provider beside Agent leaves provider selection explicit in the
agent's configuration or session provisioning.

### Hub owns messaging

An agent connects to the Hub and registers a routable instance identity. Agent
kind, such as Codex or Claude Code, describes the instance. Direct messages
address a specific instance. Publications address topics, and the Hub delivers
them to matching subscribers. An integration can publish a PR comment event
through the same topic mechanism. Registration and routing work when the Hub
runs by itself and when it runs beside Agent or Provider.

The first deployment target is local and self-hosted. The recipient decides
whether a delivery is a notification or should schedule work. Agent Control
continues to govern the lifecycle of any run. The Hub wire, authorization,
delivery guarantees, and reconnect behavior require a separate protocol draft
before implementation. Internet hosting is a later deployment target.

### The 0.2 cutover uses one set of names

- Replace the old `hub` command and positional `serve` forms with the
  configuration-driven `serve` entry point. Remove the `oapx` binary alias.
- Use Agent Service in product documentation for the existing multi-session
  daemon and reserve Hub for messaging. Update the affected public SDK types,
  SDK launchers, examples, and test harnesses together. Keep the published OXAP
  package names.
- Use `OXAP_*` for public environment settings and `oxap` for the built-in
  adapter name. Remove legacy environment and data-directory fallbacks. Document
  the migration of existing settings and state.
- Use a versioned 0.2 wire cutover for first-party identifiers such as protocol
  and profile values and transport event names. Define those changes with the
  schemas and conformance fixtures. Reject the retired wire version explicitly.
  Preserve upstream harness identifiers and historical release artifacts.

## Consequences

- Ship one server binary, as in [Decision 0038](0038-one-released-binary-and-a-library-for-every-language.md).
  Agent and Provider keep separate protocol responsibilities within it.
- Update the CLI draft, bindings, SDK launchers, and migration guide in the
  implementation PRs. This decision records the target architecture and
  supersedes the older naming policy when the 0.2 cutover ships.
- Validate the release bundle against the merged tree. Exercise a multi-adapter
  Agent service, a raw Agent and Provider connection, a standalone Hub, and an
  Agent and Hub deployment with direct and topic deliveries. Publish the 0.2.0
  release after those paths work on the packaged build.
