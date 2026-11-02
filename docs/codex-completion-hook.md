# Codex completion delivery

Configure an absolute, operator-controlled Codex executable:

```toml
codex_command = "/absolute/path/to/codex"
# Optional on Unix: use the running desktop app's private owner-routing socket.
codex_ipc_socket = "/absolute/path/to/.codex/ipc/ipc.sock"
```

## Choose delivery through MCP

Protocol 1.73 adds `codex_delivery_mode` to the configuration managed by
`server_settings`. Read settings with the administrative credential, preserve the
returned `saved` object, change this field, and save with the returned
`expected_revision`:

```json
{"codex_delivery_mode":"queued"}
```

This is the field to merge into `configuration`, not a replacement for the whole
configuration. Use `"immediate"` to deliver to an active desktop turn, or request
`codex queue --resume` when owner discovery confirms there is no active owner/turn.
Immediate mode requires a configured private `codex_ipc_socket` on Unix.
`queued` always uses `codex queue` without steering or resuming, even when IPC is
configured. There is no concurrent second turn in the same chat.

Omitting the field preserves the previous behavior described below. Changes are
hot-reloaded for subsequent dispatches; an already accepted delivery is never
replayed. `capabilities.features.codex_delivery_policy` and
`codex_hook_status.delivery_policy` report the current policy, not the delivery
method of past messages. CLI success acknowledges acceptance, not that a model
has answered; `--resume` requests continuation but cannot guarantee it.

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

When `codex_ipc_socket` is configured, delivery discovers the bound chat's existing
local desktop owner and sends `thread-follower-steer-turn` directly to that owner.
The desktop manager submits `turn/steer` against its current active turn and handles
stale turn IDs. The completion is passed as untrusted additional context; it cannot
change the chat's model, reasoning effort, permissions or service tier. The hook
adds corrective context to the same active turn: no `turn/interrupt`, competing
turn or cancellation of running commands is requested.

The socket, private parent directory and connected peer must belong to the server's
user. This desktop IPC integration is version-sensitive: it requires owner discovery
version 1 and untrusted input support, and fails closed on unsupported responses.
Legacy mode performs no socket scanning, new-chat creation or automatic chat resume.
Without the IPC setting, legacy delivery retains queue-only behavior. With IPC,
an absent owner or explicit owner-side idle guard falls back to `codex queue`
without `--resume`. Other errors, disconnects and ambiguous acknowledgements do not
fall back to the queue or cause an automatic retry.

The delivery ID is also the steering `clientUserMessageId`. Database claims prevent
duplicate dispatch. A disconnect or timeout after sending is uncertain and is never
automatically retried or queued. Read results through MCP when notified; an
acknowledgement does not prove a model consumed the message, authorize new work,
or resume a user-paused goal. Existing chat model and reasoning settings are unchanged.

Lifecycle hooks in the [manual plugin](codex-plugin.md) validate completion
messages. They do not wake idle chats or authorize builds. Ambiguous delivery
requires manual reconciliation and cannot be retried automatically; disabling a
binding stops future attempts.
