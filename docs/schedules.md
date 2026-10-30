# Schedules

In the UI: **Schedules → +**. Choose a saved pipeline or experiment, time and repeat
policy. Select a task to edit, pause, resume or cancel it.

## Schedule a saved revision

`schedules_create`

<!-- mcp: schedules_create -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "nightly",
  "pipeline": {
    "name": "checks",
    "revision": 1
  },
  "at_ms": 1893456000000,
  "misfire": "skip",
  "grace_ms": 60000,
  "repeat": {
    "every_seconds": 86400,
    "overlap": "skip"
  },
  "request_id": "nightly-1",
  "actor": "user",
  "reason": "Daily checks"
}
```

`at_ms` is UTC Unix milliseconds; this example starts at 2030-01-01 00:00 UTC.
For calendar scheduling, replace `repeat` with:

```json
{"cron":"0 2 * * *","timezone":"UTC","overlap":"skip"}
```

`schedules_list`

<!-- mcp: schedules_list -->
```json
{
  "project_id": "PROJECT_ID"
}
```

Use `schedules_get` to read the current revision before `schedules_update` or
`schedules_change`. `misfire: skip` discards missed occurrences; `run_once` admits
one eligible occurrence. The running server dispatches tasks; `run-due` is optional.
