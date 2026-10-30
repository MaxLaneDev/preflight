# Retry one stage or test

In the run graph or test tree, select **Retry**. Preview shows which upstream
artifacts can be reused. A retry creates a separate run and keeps the old result.

`pipeline_retry_preview`

<!-- mcp: pipeline_retry_preview -->
```json
{
  "project_id": "PROJECT_ID",
  "run_id": "RUN_ID",
  "stage_id": "test"
}
```

`pipeline_retry`

<!-- mcp: pipeline_retry -->
```json
{
  "selection": {
    "project_id": "PROJECT_ID",
    "run_id": "RUN_ID",
    "stage_id": "test"
  },
  "idempotency_key": "retry-tests-1",
  "actor": "user",
  "reason": "Test corrected"
}
```

Omit `source` to capture current project files. Set `test_id` for a supported
individual case, or `stage_override` for a complete replacement stage. Inspect the
preview before execution; missing or incompatible inputs prevent reuse.

Repeating an identical idempotency key returns the same retry.
