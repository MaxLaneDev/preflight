# Lists and live updates

Projects, pipelines, experiments and evidence load more items as you scroll. Search
runs on server-side data, not only the page already loaded in the browser.

For MCP lists:

```json
{"project_id":"PROJECT_ID","limit":20,"before":"NEXT_CURSOR"}
```

Pass each tool's returned cursor using its documented field (`before` or `after`).
Changing filters starts a new query. Lists are live views; use recorded run IDs
when you need stable references.

The browser shares SSE through a SharedWorker where supported. Unsupported
browsers use bounded polling. Hiding/closing pages releases subscriptions; stream
limits and reconnect backoff prevent one stream per tab from exhausting the server.
