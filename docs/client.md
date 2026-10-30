# Web UI

Open `http://127.0.0.1:4170`. The server includes the interface and MCP.

| Want to… | Open |
| --- | --- |
| Edit or run a configuration | Project → Pipelines |
| Inspect one execution | Pipeline → Run |
| Open a test's output or source | Run → Tests → test |
| Inspect dependencies | Run → Graph |
| Read changing output | Run → Logs |
| Compare captured code | Run → Changes |
| Copy workspace/input/output paths | Run → Paths |
| Compare alternatives | Project → Experiments |
| See all current work | Execution |
| Configure the server | Gear icon → Settings |

Use the theme buttons in the header. Copy buttons appear on hover/focus. Lists load
as you scroll; panels scroll independently. Hidden pages release live subscriptions.

## Compatibility client

For a separate UI listener against an existing server:

```sh
preflight-client --server http://127.0.0.1:4170 --bind 127.0.0.1:4172
```

This is a compatibility tool; normal installation needs only the server.
