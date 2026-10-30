# Codex completion delivery

Configure an absolute, operator-controlled Codex executable:

```toml
codex_command = "/absolute/path/to/codex"
```

`codex_hook_bind`

<!-- mcp: codex_hook_bind -->
```json
{
  "run_id": "RUN_ID",
  "thread_id": "ORIGINATING_THREAD_ID"
}
```

`codex_hook_status`

<!-- mcp: codex_hook_status -->
```json
{
  "run_id": "RUN_ID"
}
```

Delivery uses `codex queue` for that explicit run/chat binding. A delivery ID
identifies duplicates. Read results through MCP when notified; a CLI acknowledgement
does not prove a model consumed the message.

Lifecycle hooks in the [manual plugin](codex-plugin.md) validate completion
messages. They do not wake idle chats or authorize builds. Retry ambiguous delivery
only after inspecting status; disabling a binding stops future attempts.
