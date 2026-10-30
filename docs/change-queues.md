# Change queues

## Run independent commits in parallel

`queue_create`

<!-- mcp: queue_create -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "compare",
  "request_id": "compare-1",
  "mode": "independent",
  "max_parallel": 2,
  "definition": {
    "version": 1,
    "pipeline": {
      "stages": [
        {
          "id": "test",
          "kind": "test",
          "command": {
            "program": "go",
            "args": [
              "test",
              "./..."
            ]
          }
        }
      ]
    }
  },
  "changes": [
    {
      "id": "before",
      "source": {
        "commit": "FULL_COMMIT_A"
      }
    },
    {
      "id": "after",
      "source": {
        "commit": "FULL_COMMIT_B"
      }
    }
  ]
}
```

Use full local Git commit IDs, for example from `git rev-parse HEAD`.

`queue_status`

<!-- mcp: queue_status -->
```json
{
  "queue_id": "QUEUE_ID"
}
```

`queue_cancel`

<!-- mcp: queue_cancel -->
```json
{
  "queue_id": "QUEUE_ID",
  "actor": "user",
  "reason": "Stop this comparison"
}
```

## Apply patches sequentially

Set `mode: "cumulative"`, `max_parallel: 1`, and use the same base commit for every
item. Each source adds its own `patches: [{"name":"fix","diff":"PATCH_TEXT"}]`.
The second item includes the first item's patches. Failure blocks later items.

`independent` with parallelism 1 instead tests separate versions serially.
The checkout and index stay unchanged. Definitions are immutable; repeating the
same request ID and body returns the original queue.

Limits: 32 active queues, 32 items, 64 KiB request. Queue sources currently use
single-repository exact Git commits/patches. Read per-run completion events.
