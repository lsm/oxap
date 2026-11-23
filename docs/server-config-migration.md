# Migrate to a server configuration

Run `oxap serve --config server.json`. The document can be JSON or the
supported YAML subset. Each top-level service section selects its binding.

| Previous command | Service config |
| --- | --- |
| `oapx serve agent --stdio` | Set `agent.stdio: raw` and select a type `oapx` entry from `agent.adapters` with `agent.adapter`. |
| `oapx serve agent --backend NAME --config registry.json` | Set `agent.stdio: raw` and `agent.adapter: NAME`. Copy `adapters` and `tool_sources` from the registry into `agent`. |
| `oapx serve provider --stdio` | Set `provider.stdio: raw`. |
| `oapx serve provider --http 127.0.0.1:6271` | Set `provider.http: "127.0.0.1:6271"`. |
| `oapx serve agent,provider --stdio` | Configure both services with raw stdio below. |
| `oxap hub --stdio --config registry.json` | Set `agent.stdio: multi_session` and move the registry entries into `agent`. |
| `oxap hub --addr 127.0.0.1:6270 --config registry.json` | Set `agent.http: "127.0.0.1:6270"` and move the registry entries into `agent`. See the [Agent Service example](../examples/agent-service.json). |

The SDKs generate this config for their default child and remove it after
startup. Use an `oxap` build that supports `serve --config`. The SDKs pass custom argument lists through as supplied. The legacy CLI
forms still accept their own options, including `--model`, `--specimens` and
the hub's history and parking options.

```json
{
  "agent": {
    "stdio": "raw",
    "adapter": "builtin",
    "adapters": { "builtin": { "type": "oapx" } }
  },
  "provider": { "stdio": "raw" }
}
```

The wire remains OAP 0.1. The `agent` and `provider` profiles route on one pipe
when both bindings are raw. The [Agent Service](../drafts/agent-service.md)
uses a separate multi-session binding and owns its stdio pipe exclusively.

The multi-session client type is available as `AgentServiceClient` in
`go/client` and `@oxap/client`. Existing client names remain valid.
