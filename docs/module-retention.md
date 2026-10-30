# Keep a module

`module_pin_set`

<!-- mcp: module_pin_set -->
```json
{
  "project_id": "PROJECT_ID",
  "sha256": "SHA256",
  "owner": "release",
  "pinned": true,
  "expected_revision": 0,
  "request_id": "keep-library-1",
  "actor": "user",
  "reason": "Required by release builds"
}
```

`module_retention`

<!-- mcp: module_retention -->
```json
{
  "project_id": "PROJECT_ID",
  "sha256": "SHA256"
}
```

To release this owner, set `pinned: false` with its returned revision and a new
request ID. Other owners and active consumers continue to protect the module.
Exact request replay returns the stored result; unpinning deletes no bytes.
Use a fresh [GC preview](storage-gc.md) for physical removal.
