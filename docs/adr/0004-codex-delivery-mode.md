# Codex completion delivery policy

Status: accepted

A completion can be queued for later work or delivered to an active Codex turn.
Make that choice explicit with the hot-reloaded `codex_delivery_mode` setting:
`queued` bypasses desktop IPC; `immediate` uses private owner-routed IPC and asks
the CLI to resume only after a definite idle/no-owner response. An omitted setting
keeps the legacy behavior for existing installations.

Read one configuration snapshot per dispatch. Never resend an uncertain delivery
through another transport. Keep bindings, delivery IDs and the originating-chat
restriction unchanged. No database migration is needed: this is dispatch policy,
not a rewrite of run history. MCP protocol 1.73 advertises the setting and reports
its current value; it does not claim a past delivery used the current policy.
