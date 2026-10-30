# Multiple repositories in one run

Register both projects. Select full local Git commit IDs and non-overlapping mount paths:

`pipeline_start_multi`

<!-- mcp: pipeline_start_multi -->
```json
{
  "project_id": "APP_PROJECT_ID",
  "definition": {
    "version": 1,
    "pipeline": {
      "stages": [
        {
          "id": "test",
          "kind": "test",
          "cwd": "app",
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
    "roots": [
      {
        "name": "app",
        "project_id": "APP_PROJECT_ID",
        "path": "app",
        "revision": {
          "mode": "git",
          "source": {
            "commit": "APP_COMMIT_ID",
            "patches": []
          }
        }
      },
      {
        "name": "library",
        "project_id": "LIBRARY_PROJECT_ID",
        "path": "library",
        "revision": {
          "mode": "git",
          "source": {
            "commit": "LIBRARY_COMMIT_ID",
            "patches": []
          }
        }
      }
    ]
  }
}
```

The command runs in `app`; the other source is mounted as `library`.
Use `revision: {"mode":"dirty","source":{"untracked":[]}}` for checked working bytes.

`runs_source` returns named roots and their aggregate digest.
`runs_source_root_inputs` pages one root's captured paths; patch lookup accepts
`root_name`. A run accepts 1–8 roots. If any root fails preparation, the combined
run fails without changing the original repositories or indexes.
