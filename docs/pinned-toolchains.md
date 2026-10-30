# Pin a compiler or SDK

Observe the actual permitted file/directory first:

`toolchain_inspect_pin`

<!-- mcp: toolchain_inspect_pin -->
```json
{
  "project_id": "PROJECT_ID",
  "path": "/absolute/path/to/compiler",
  "kind": "executable"
}
```

`toolchain_inspect_pin`

<!-- mcp: toolchain_inspect_pin -->
```json
{
  "project_id": "PROJECT_ID",
  "path": "/absolute/path/to/sdk",
  "kind": "directory"
}
```

Copy returned path/SHA-256 pairs into a profile's pins:

```json
{"pins":{"target":"TARGET_TRIPLE","tools":{"cc":{"path":"COMPILER_PATH","sha256":"COMPILER_SHA256"}},"sdks":{"sdk":{"path":"SDK_PATH","sha256":"SDK_SHA256"}}}}
```

Tools must match their profile aliases. Use `${pinned.cc}` and `${sdk.sdk}` as
whole argv/environment values. Changes to pinned bytes or incompatible host/target,
ABI or SDK contracts fail before execution. Private copies remain the default.

For a loader-sensitive operator tool, `run_in_place: true` requires a content pin
and allowed original path; launch rechecks it. Ambient libraries remain outside
the pin. A pin is not a sandbox or a complete runtime closure.

`runs_toolchain`

<!-- mcp: runs_toolchain -->
```json
{
  "run_id": "RUN_ID"
}
```

See [modules](prepared-modules.md) and [cache](build-cache.md).
