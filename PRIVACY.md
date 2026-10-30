# Privacy

Preflight stores sources, configuration, logs, test results and artifacts on the
server's disk. It includes no publisher analytics.

| Connection | Data sent |
| --- | --- |
| Web UI / MCP | Requests and results to your configured server |
| Worker | Assigned job inputs; returns logs and declared outputs |
| External CI | Requests to operator-configured providers |
| Release notifications | Public GitHub repository/release requests, only when enabled |

Pipeline commands have their execution environment's network and filesystem
permissions. Your coding client's/model provider's policies also apply to data
you send through that client.

Cleanup, Trash and archive controls manage retained data. Uninstalling preserves
configuration/data; remove them separately when intended. Never post credentials
or private project output in public reports.
