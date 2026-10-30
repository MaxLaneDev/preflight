# Captured Git sources

Get a full local commit ID:

```sh
git rev-parse HEAD
```

`pipeline_start_git`

<!-- mcp: pipeline_start_git -->
```json
{
  "project_id": "PROJECT_ID",
  "definition": {
    "version": 1,
    "pipeline": {
      "stages": [
        {
          "id": "test",
          "kind": "test",
          "command": {
            "program": "make",
            "args": [
              "test"
            ]
          }
        }
      ]
    }
  },
  "source": {
    "commit": "FULL_COMMIT_ID",
    "patches": []
  }
}
```

Add ordered patches as `[{"name":"fix","diff":"UNIFIED_DIFF"}]`. Runs use an
isolated snapshot; the checkout/index are not modified. Git links/submodules and
unsafe paths are rejected.

## Checked dirty working tree sources

`pipeline_start_dirty`

<!-- mcp: pipeline_start_dirty -->
```json
{
  "project_id": "PROJECT_ID",
  "definition": {
    "version": 1,
    "pipeline": {
      "stages": [
        {
          "id": "test",
          "kind": "test",
          "command": {
            "program": "make",
            "args": [
              "test"
            ]
          }
        }
      ]
    }
  },
  "source": {
    "expected_head": "FULL_COMMIT_ID",
    "untracked": [
      "new-file.c"
    ]
  }
}
```

Tracked working bytes/deletions are captured. Untracked files require explicit
selection; ignored paths are not an implicit allowlist. A wrong expected HEAD or
unsafe selection fails instead of switching sources.

`runs_source`

<!-- mcp: runs_source -->
```json
{
  "run_id": "RUN_ID"
}
```

`runs_source_patch`

<!-- mcp: runs_source_patch -->
```json
{
  "run_id": "RUN_ID",
  "ordinal": 0
}
```

Compare captured inputs in non-Git runs:

`runs_compare_inputs`

<!-- mcp: runs_compare_inputs -->
```json
{"run_id":"RUN_ID","baseline_id":"BASELINE_RUN_ID","offset":0}
```

A hash identifies bytes; it cannot recreate an absent text patch.
[Multiple roots](multi-sources.md).
