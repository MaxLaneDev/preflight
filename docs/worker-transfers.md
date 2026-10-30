# Verified transfers

Worker stages automatically transfer their frozen source and declared results.
Inspect the recorded run:

`runs_source`

<!-- mcp: runs_source -->
```json
{
  "run_id": "RUN_ID"
}
```

`runs_worker_jobs`

<!-- mcp: runs_worker_jobs -->
```json
{
  "run_id": "RUN_ID"
}
```

Transferred trees validate paths, file sizes, hashes and executable metadata.
Symlinks and escaping paths are rejected. Parent ownership and operation leases
protect files until collection/cleanup settles. Transfer identity alone does not
publish an artifact or establish toolchain compatibility.

Use normal pipeline inputs/outputs; do not share a host directory between unrelated
workers. [Stage configuration](remote-pipelines.md).
