# Toolchain profiles

Save reusable command aliases; no language identifier is required.

`toolchain_save`

<!-- mcp: toolchain_save -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "native",
  "expected_revision": 0,
  "definition": {
    "version": 1,
    "tools": {
      "cc": {
        "program": "cc",
        "probe_args": [
          "--version"
        ]
      }
    }
  }
}
```

Use the alias in a pipeline:

```yaml
command: {program: "${tool.cc}", args: [main.c, -o, app]}
```

`toolchain_probe`

<!-- mcp: toolchain_probe -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "native",
  "revision": 1
}
```

Probes run only when requested and return bounded evidence. Profile revisions are
immutable under `.preflight/toolchains/`. Saved runs choose an explicit revision.
For compiler/SDK identity and compatible modules, add [pins](pinned-toolchains.md).
