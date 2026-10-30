# Experiments

Compare a change against a baseline run without replacing the original results.
In the UI, choose **Experiments → +**, select a pipeline revision and optional baseline.

## Save and run

`experiment_save`

<!-- mcp: experiment_save -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "candidate",
  "expected_revision": 0,
  "definition": {
    "version": 1,
    "pipeline": {
      "name": "checks",
      "revision": 1
    },
    "baseline_run": "BASELINE_RUN_ID"
  }
}
```

`experiment_start`

<!-- mcp: experiment_start -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "candidate",
  "revision": 1,
  "idempotency_key": "candidate-1"
}
```

`experiment_compare`

<!-- mcp: experiment_compare -->
```json
{
  "project_id": "PROJECT_ID",
  "run_id": "RUN_ID"
}
```

An experiment can select a Git/dirty/multi-repository source, toolchain profile,
fixture cases or a complete pipeline override. Use `experiment_get` to inspect
the captured revision. Changes create a new revision.

History stays under its project and experiment. Missing test evidence is unknown;
comparison does not invent counts or performance claims. [Source selection](git-sources.md).
