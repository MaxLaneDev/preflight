# Worker runtime evidence

`runs_stage_key`

<!-- mcp: runs_stage_key -->
```json
{
  "run_id": "RUN_ID",
  "stage_id": "build"
}
```

Updated workers can return `preflight-worker-runtime-v1` observations: actual
OS/architecture, launch environment hash, launcher hash where observable, and
container image/platform/user selection. Secret values are not included.

Remote v2 diagnostic keys retain the worker identity/generation and verified input
transfer. Old workers may have no receipt; absence is unknown. Receipts remain in
job evidence after workspace cleanup.

An executable hash does not cover dynamic libraries or every tool installed by
setup. These observations do not authorize remote cache reuse or remote-container
module publication. [Cache policy](build-cache.md).
