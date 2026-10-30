# Execution view

Open **Execution** to inspect work across projects. Filter active, planned or recent
items, then select a pipeline/experiment to open its run. Cancel acts on the selected
run, queue or task. History remains under its original project.

`scheduler_status`

<!-- mcp: scheduler_status -->
```json
{}
```

The bottom bar shows executing and planned progress. These are live observations,
not resource reservations. Server command slots and memory admission still apply.

For one run, use `build_report`; for a change queue, use `queue_status`.
[Limits](server-settings.md) · [Schedules](schedules.md) · [Queues](change-queues.md).
