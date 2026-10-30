# Build cache

Caching is opt-in for deterministic build stages with complete declared inputs and
a pinned toolchain profile. Tests run again.

```yaml
cache:
  pipeline_id: compiler
  deterministic: true
  inputs_complete: true
```

Use [pinned tools](pinned-toolchains.md); setup commands and remote stages do not
use this cache. Only successful stages of successful runs are candidates. Hits
verify and copy outputs into the new run. Corruption forces a rebuild.

`runs_cache`

<!-- mcp: runs_cache -->
```json
{
  "run_id": "RUN_ID"
}
```

`runs_stage_key`

<!-- mcp: runs_stage_key -->
```json
{
  "run_id": "RUN_ID",
  "stage_id": "build",
  "baseline_id": "BASELINE_RUN_ID"
}
```

Keys include captured inputs, configuration, pinned toolchain/SDK/target, host and
artifacts. A diagnostic identity alone never grants cache reuse. Undeclared files,
network state or dynamic libraries must be covered by the author's contract.

## Sharing concurrent build execution

Add `single_flight: true` to the cache policy. Identical eligible requests share
one execution. Cancelling one subscriber preserves the others; cancelling the
last stops the execution. Failed work is not cached.

## Invalidate entries

`cache_cleanup_preview`

<!-- mcp: cache_cleanup_preview -->
```json
{
  "project_id": "PROJECT_ID",
  "pipeline_id": "compiler"
}
```

`cache_cleanup_execute`

<!-- mcp: cache_cleanup_execute -->
```json
{
  "plan_id": "PLAN_ID"
}
```

Invalidation keeps producer evidence and deletes no bytes. Physical cleanup uses
[retention-aware plans](storage-gc.md). Cache namespaces and UI pipeline keys are distinct.
