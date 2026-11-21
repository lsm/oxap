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

Messages are hints: recipients must verify authoritative state before acting on
an integration event. The first profile is an ephemeral, local service. A restart
loses registrations, subscriptions, and queued deliveries. There is no offline
mailbox, durable storage, replay, wildcard subscription, presence history, or
exactly-once guarantee. A later network deployment requires a separately
specified authenticated binding and authorization model; it cannot expose this
local binding on the public internet.

## Connection and identity

The first local binding is a Unix-domain stream socket in an owner-only
directory. The Hub and clients verify that the directory and socket belong to
their effective operating-system user and that other users have no access. The
directory must be owned by that user with mode `0700`; neither side follows a
symlink, and a wrong owner or mode is an error, not something to repair. Each
side also checks the connected peer credentials and requires the peer user ID
to equal its own effective user ID. It fails closed if credentials are
unavailable or differ. These checks are required on macOS and Linux; another
platform, including Windows, needs a separately specified secure binding.

The Hub holds an exclusive lock in the protected directory for its entire
lifetime. Only the lock holder may remove a stale socket, bind or remove the
socket on shutdown. A second server fails without disturbing the first. A socket
path too long for the platform fails with a diagnostic; no silent fallback to a
less protected directory is allowed. A co-hosted Agent or Provider service does not register clients automatically. Remote deployment and its
transport authentication require a separate binding.

Each connection registers exactly once before any other operation. The Hub
allocates an opaque `instance_id` containing at least 128 random bits from a
cryptographically secure source, unique among active instances. The client
supplies a display name and a kind such as `codex`, `claude-code`,
or `integration`; neither is an identity or proof of ownership. A registration
ends when its connection closes. Reconnecting creates a new instance ID and
requires registering and subscribing again. The Hub must not accept a
client-supplied sender ID as authority. A restarted Hub must not reuse instance
IDs deliberately. IDs are opaque strings; clients must not parse them.

Every peer able to connect to the socket is authorized to list instances,
subscribe to any topic, send and publish. No finer-grained access control is
provided. This boundary does not distinguish processes of the same account,
including agent tool code. A self-declared kind or name does not authenticate an
integration. Recipients must independently verify any event they act on. The
Hub must not log payloads or metadata.

## Wire and operations

Each UTF-8 line contains exactly one JSON object followed by a newline. Blank
lines, carriage returns before the newline, duplicate JSON members, invalid
UTF-8 and lines over the limit close the connection. Protocol output carries no
logs or banners. The binding does not accept ancillary file descriptors. Every
envelope has `protocol: "oxap"`, `version: "0.2"`, and
`profile: "oxap.hub-messaging"`. A request has `id`, `type`, and `payload`;
a response has its own `id`, `type`, `in_reply_to`, and `payload`. An event
has `id`, `type`, and `payload`, and no `in_reply_to`. Envelope IDs are unique
per connection and direction. A repeated request ID closes the connection.
The server checks protocol and version before profile routing; a correlatable
unsupported version receives `error.response` with `unsupported_version`.
Unknown members and operations are refused rather than silently ignored.

The server reserves a response slot before accepting each request and returns
exactly one response unless the connection fails. Responses use a bounded queue
separate from event deliveries. The Hub stops reading requests on a connection
while no response slot is free, while continuing to watch for hang-up; it never
blocks work on other connections. Clients must read concurrently with writes to
avoid a backpressure deadlock. This socket binding deliberately applies
backpressure, unlike the existing Agent Service pipe binding. No heartbeat is
required: a closed socket ends the registration, while an unresponsive but open
peer remains subject to bounded queues. Responses and events can interleave,
and an event may precede the response to the operation that caused it. Requests
on one connection are processed in wire order. Clients correlate responses with
`in_reply_to`, not arrival order. A connection that has not registered by the
server's advertised registration deadline is closed. The binding declares
finite frame, payload, connection, subscription and outbound queue limits.

| Request | Payload | Successful response payload |
| --- | --- | --- |
| `hub.register.request` | `name`, `kind` | `instance_id`, `limits` |
| `hub.instances.list.request` | empty object | `instances`: active `instance_id`, `name`, `kind` entries |
| `hub.subscribe.request` | `topic` | `topic` |
| `hub.unsubscribe.request` | `topic` | `topic` |
| `hub.send.request` | `recipient_id`, `message` | `message_id` |
| `hub.publish.request` | `topic`, `message` | `message_id`, `admitted_count`, `dropped_count` |

A topic is a nonempty, case-sensitive UTF-8 string, compared exactly; `*` has
no special meaning. An instance has at most one subscription to a given topic.
Subscribing again succeeds without creating a duplicate; unsubscribing an absent
topic succeeds. A publication includes the sender if it subscribes to the
topic. A successful subscribe response is a barrier: later admissions see the
subscription.
A message is a JSON object with required `content_type` and `content` members.
`content_type` is a nonempty string. `content` may be any JSON value, including
an object carrying integration-specific fields. Its meaning and any further
validation belong to the clients. Clients must not execute instructions merely
because they arrived through Hub.

A message may include `reply_to_message_id` for application-level correlation;
it does not acknowledge processing. For every accepted send or publish the Hub
allocates a `message_id`. A `hub.message` event contains `message_id`, the
Hub-assigned `sender_id`, the
original `message`, and exactly one of `recipient_id` or `topic`. The Hub never
rewrites the content. A receiver may use `message_id` to correlate its own logs;
it is not an idempotency key or a replay cursor.

## Admission, ordering and failure

A direct send succeeds only if the recipient is registered and the delivery was
placed in its bounded outbound queue. Admission never waits for queue space. A
full direct recipient returns `recipient_busy` with no delivery. A publication
takes a snapshot of active matching subscriptions, then independently attempts
to enqueue to each. A full subscriber is skipped and counted as dropped; its
connection and previously queued deliveries remain intact. Other subscribers can still receive
the message. The response reports both `admitted_count` and `dropped_count` for that snapshot; their sum equals the
number of matching subscribers in the snapshot. A publication with no
subscribers succeeds with both counts zero. These counts report queue admission,
not receipt or processing. Admission and disconnects are serialized so the
snapshot excludes registrations already removed; the operation cannot wait on
network writes. This best-effort fan-out is not atomic across recipients.

The Hub keeps one bounded, coalesced pending-drop notice per subscription. When
a publication skips that subscription, the notice becomes pending; once the
subscriber can read again, `hub.deliveries.dropped` reports its topic and that
one or more deliveries were missed. Multiple drops can share one notice. An
unsubscribe or disconnect can discard its pending notice; on reconnect the
client must reconcile the authoritative source after subscribing again.

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
Listing instances includes the caller and is a snapshot, not a reservation: a
listed recipient may unregister before a send. After reconnect, peers can poll
the list to discover active instances; names and kinds are advisory and may be
duplicated. Closing a connection removes its subscriptions and registration and discards its queued deliveries.

A failed operation returns `error.response` with `in_reply_to` and a payload
containing an `error` object with `code` and human-readable `message`. The
message must not contain message content.
The initial codes are `invalid_request`, `not_registered`,
`already_registered`, `recipient_not_found`, `recipient_busy`,
`resource_exhausted`, `unsupported_version`, and `internal_error`. An unknown
operation is `invalid_request`. A rejected operation has no routing
effects; a successfully admitted publication can still report dropped
subscribers. The server must not expose internal paths or secrets in errors. A
request without a usable ID cannot receive a correlated error and
closes the connection.

## Binding limits

For interoperability the protocol schema will fix upper bounds for encoded
frames, messages, IDs, names, kinds, topics, registrations and subscriptions.
A frame limit applies to the complete encoded line, including the envelope and
newline. The message limit applies to the encoded `message` object; the Hub
must reserve enough frame overhead for the largest delivery it accepts. The
instance-list response must fit the frame limit or use bounded pagination. The
Hub advertises actual limits on registration, and registration itself must fit
a fixed protocol minimum before that response is known. The exact finite limits
and the notice payload are also binding requirements
to be fixed in the schema and conformance suite; they are not yet specified by
this draft.

## Example

```json
{"protocol":"oxap","version":"0.2","profile":"oxap.hub-messaging","id":"r1","type":"hub.register.request","payload":{"name":"reviewer","kind":"codex"}}
{"protocol":"oxap","version":"0.2","profile":"oxap.hub-messaging","id":"s1","type":"hub.register.response","in_reply_to":"r1","payload":{"instance_id":"i-1c2d3e4f5061728394a5b6c7d8e9fa0b","limits":{"frame_bytes":1048576,"message_bytes":65536,"outbound_messages":256,"connections":128,"subscriptions":64,"registration_ms":5000}}}
{"protocol":"oxap","version":"0.2","profile":"oxap.hub-messaging","id":"r2","type":"hub.subscribe.request","payload":{"topic":"pr.comments"}}
{"protocol":"oxap","version":"0.2","profile":"oxap.hub-messaging","id":"s2","type":"hub.subscribe.response","in_reply_to":"r2","payload":{"topic":"pr.comments"}}
{"protocol":"oxap","version":"0.2","profile":"oxap.hub-messaging","id":"e1","type":"hub.message","payload":{"message_id":"m-9","sender_id":"i-93d41f2a5566778899aabbccddeeff00","topic":"pr.comments","message":{"content_type":"application/json","content":{"pr":42,"event":"commented"}}}}
```

The values in `limits` are examples, not fixed protocol defaults.

## Release gate

Before an implementation claims this profile, its wire schema, remaining
binding limits and conformance cases must be specified and checked against this
draft. Tests must cover two instances of the same kind, direct routing, exact
topic matching, self-publication, zero subscribers, queue exhaustion with
reported partial publication and drop notices, backpressure, disconnect and
reconnect, subscription barriers, malformed frames, peer credentials, socket
locking and path ownership. No Hub config is accepted before the service is implemented. Agent and Provider can ship
independently under Decision 0070.
