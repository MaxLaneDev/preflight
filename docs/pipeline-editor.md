# Pipeline editor

Open a project, select **+ Pipeline**, and choose **Visual** or **Code**.

```json
{
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
}
```

Code mode provides completion, formatting, undo/redo and validation errors. Use
**Validate and preview**, then **Save revision**. A new draft stays editable before
validation; required fields must be filled before saving.

**Revisions** opens saved history. Restoring an older definition creates a new
revision; existing runs keep their original configuration. Revert draft discards
only unsaved edits. [MCP save/run examples](mcp-configuration.md).
