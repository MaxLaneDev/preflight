# Reuse a prepared module

Publish a declared artifact from a successful run:

`modules_publish`

<!-- mcp: modules_publish -->
```json
{
  "run_id": "RUN_ID",
  "stage_id": "build",
  "artifact_name": "library"
}
```

Use its returned SHA-256 in a later stage:

```yaml
inputs:
  deps/library.a: "${module.SHA256}"
command:
  program: cc
  args: [main.c, "${module.SHA256}", -o, app]
```

Replace `SHA256` with the digest, preserving the complete reference. Directory
modules work as inputs/arguments, not executable programs.

`modules_get`

<!-- mcp: modules_get -->
```json
{
  "project_id": "PROJECT_ID",
  "sha256": "SHA256"
}
```

Compatibility uses exact pinned profile or container contracts. Legacy publication
is explicitly unverified. Modules and active consumers protect retained copies.
[Pin retention](module-retention.md) · [Cleanup](storage-gc.md).
