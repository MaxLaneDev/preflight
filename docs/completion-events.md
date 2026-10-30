# Completion events

Read durable project events after starting a run.

`events_read`

<!-- mcp: events_read -->
```json
{
  "project_id": "PROJECT_ID",
  "after": 0,
  "limit": 20
}
```

Save `next_cursor` and pass it as `after`. Notifications are hints; durable events
and `build_report` are authoritative. Resource subscriptions are available to MCP
clients that support them.

For Codex delivery, explicitly bind the run to its originating chat:

`codex_hook_bind`

<!-- mcp: codex_hook_bind -->
```json
{
  "run_id": "RUN_ID",
  "thread_id": "ORIGINATING_THREAD_ID"
}
```

See [Codex delivery](codex-completion-hook.md). Do not bind unrelated chats.
