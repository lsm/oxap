# Hub messaging protocol

Status: proposed draft for review; not implemented
Protocol: OXAP 0.2, Hub messaging
Authority: [Decision 0070](../decisions/0070-one-server-with-agent-provider-and-hub-services.md)

## Purpose and scope

Hub lets separately running agents and integrations exchange direct messages
and topic publications. An instance is one connected client, not an agent kind:
several Codex instances may coexist. A GitHub integration is a client too. Hub
routes messages; it does not start an Agent Control run, interpret message
content, or associate a registration with an Agent Control session. The
recipient decides whether to notify a user or schedule work.

The first profile is an ephemeral, local, single-user service. A restart loses
registrations, subscriptions, and queued deliveries. There is no offline
mailbox, durable storage, replay, wildcard subscription, presence history, or
exactly-once guarantee. A later network deployment requires a separately
specified authenticated binding and authorization model; it cannot expose this
local binding on the public internet.

## Connection and identity

The local binding uses a WebSocket at an explicitly configured loopback address.
It accepts only loopback peers and allowlisted loopback `Host` values. It rejects
browser connections carrying an `Origin` header; this binding does not authorize
web pages. The server rejects non-loopback configuration rather than silently
exposing an unauthenticated endpoint. A co-hosted Agent or Provider service does
not register clients automatically. WebSocket TLS, remote identity and delegated
authority are outside this binding.

Each connection registers exactly once before any other operation. The Hub
allocates an opaque, unpredictable `instance_id` unique among active instances.
The client supplies a display name and a kind such as `codex`, `claude-code`,
or `integration`; neither is an identity or proof of ownership. A registration
ends when its connection closes. Reconnecting creates a new instance ID and
requires registering and subscribing again. The Hub must not accept a
client-supplied sender ID as authority. A restarted Hub must not reuse instance
IDs deliberately. IDs are opaque strings; clients must not parse them.

The local binding trusts all accepted local clients equally. Any connected
client can enumerate registrations, subscribe to any topic and address any
instance. Payloads and metadata must not be logged by the Hub. A loopback listener does not distinguish local operating-system users. No
confidentiality or authorization is promised against any local process that can
connect; deployments requiring user isolation need an authenticated binding.

## Wire and operations

Each WebSocket text message contains exactly one UTF-8 JSON object. Binary
frames, invalid JSON, duplicate object members and messages over the advertised
size limit are protocol errors. Requests have `id`, `type`, and `payload`;
responses have their own `id`, `type`, `in_reply_to`, and `payload`. An event
has `id`, `type`, and `payload`, and no `in_reply_to`. IDs are opaque strings;
request IDs must be unique among outstanding requests on that connection. The
server returns exactly one response per accepted request; it may interleave
responses and delivery events. Requests arriving on one connection are processed
in wire order. Clients correlate responses with `in_reply_to`, not their order.
The binding declares a finite message limit and finite queue and connection
limits. A connection closed for invalid framing may receive no response.

| Request | Payload | Successful response payload |
| --- | --- | --- |
| `hub.register` | `name`, `kind` | `instance_id`, `limits` |
| `hub.instances.list` | empty object | `instances`: active `instance_id`, `name`, `kind` entries |
| `hub.subscribe` | `topic` | `topic` |
| `hub.unsubscribe` | `topic` | `topic` |
| `hub.send` | `recipient_id`, `message` | `message_id` |
| `hub.publish` | `topic`, `message` | `message_id`, `recipient_count` |

A topic is a nonempty, case-sensitive UTF-8 string, compared exactly; `*` has
no special meaning. An instance has at most one subscription to a given topic.
Subscribing again succeeds without creating a duplicate; unsubscribing an absent
topic succeeds. A publication includes the sender if it subscribes to the topic.
A message is a JSON object with required `content_type` and `content` members.
`content_type` is a nonempty string. `content` may be any JSON value, including
an object carrying integration-specific fields. Its meaning and any further
validation belong to the clients. Clients must not execute instructions merely
because they arrived through Hub.

For every accepted send or publish the Hub allocates a `message_id`. A
`hub.message` event contains `message_id`, the Hub-assigned `sender_id`, the
original `message`, and exactly one of `recipient_id` or `topic`. The Hub never
rewrites the content. A receiver may use `message_id` to correlate its own logs;
it is not an idempotency key or a replay cursor.

## Admission, ordering and failure

A direct send succeeds only if the recipient is registered and the delivery was
placed in its bounded outbound queue. A publication takes a snapshot of active
matching subscriptions and admits the delivery to every matching queue as one
atomic operation, or admits none and returns `resource_exhausted`. A publication
with no subscribers succeeds with `recipient_count: 0`. The response counts the
snapshot admitted, not clients that received or processed the event. The Hub
must serialize admission and disconnects so a snapshot does not include a
registration already removed. Admission has a finite deadline; overload returns
an error instead of waiting indefinitely. A disconnected or full recipient
cannot cause an unbounded queue or block other operations indefinitely.

An accepted delivery is attempted at most once on that connection. Success
means queued, not written, received, processed or persisted. A disconnect,
write failure, process crash or restart may lose an accepted delivery. The Hub
does not retry or redeliver across connections. A client retry after an unknown
outcome is a new operation and can duplicate a message. Applications that need
stronger guarantees must define their own acknowledgments and idempotency above
this protocol.

For one recipient, deliveries admitted by the Hub are emitted in admission
order. There is no global order visible across recipients and no causal order
between concurrent senders. A successful `hub.unsubscribe` response is a
barrier: publications admitted after that operation cannot select that
subscription. Events admitted earlier may still arrive after the response.
Listing instances is a snapshot, not a reservation: a listed recipient may
unregister before a send. Closing a connection removes its subscriptions and
registration and discards its queued deliveries.

A failed operation returns `hub.error` with `in_reply_to` and a payload containing
`code` and a human-readable `message` that must not contain message content.
The initial codes are `invalid_request`, `not_registered`,
`already_registered`, `recipient_not_found`, `resource_exhausted`, and
`internal_error`. An unknown operation is `invalid_request`. A rejected operation
has no routing effects. The server must not expose internal paths or secrets in
errors. A request without a usable ID cannot receive a correlated error and
closes the connection.

## Example

```json
{"id":"r1","type":"hub.register","payload":{"name":"reviewer","kind":"codex"}}
{"id":"s1","type":"hub.register.response","in_reply_to":"r1","payload":{"instance_id":"i-7","limits":{"message_bytes":65536,"outbound_messages":256,"connections":128}}}
{"id":"r2","type":"hub.subscribe","payload":{"topic":"pr.comments"}}
{"id":"s2","type":"hub.subscribe.response","in_reply_to":"r2","payload":{"topic":"pr.comments"}}
{"id":"e1","type":"hub.message","payload":{"message_id":"m-9","sender_id":"i-3","topic":"pr.comments","message":{"content_type":"application/json","content":{"pr":42,"event":"commented"}}}}
```

The values in `limits` are examples. The implementation advertises its actual
finite limits, including the admission deadline, in the registration response.

## Release gate

Before an implementation claims this profile, its wire schema, version
negotiation and conformance cases must be specified and checked against this
draft. Tests must cover two instances of the same kind, direct routing, exact
topic matching, self-publication, zero subscribers, queue exhaustion with no
partial publication, disconnect and reconnect, subscription barriers, malformed
frames, and the local binding's network and browser restrictions. No Hub config
is accepted before the service is implemented. Agent and Provider can ship
independently under Decision 0070.
