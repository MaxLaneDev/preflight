# Archive a run

Create a portable copy before expiring local payloads:

`archives_create`

<!-- mcp: archives_create -->
```json
{
  "project_id": "PROJECT_ID",
  "run_id": "RUN_ID",
  "request_id": "archive-1",
  "actor": "user",
  "reason": "Keep this result",
  "logs": true,
  "artifacts": true,
  "source": true,
  "environment": false
}
```

`archives_verify`

<!-- mcp: archives_verify -->
```json
{
  "project_id": "PROJECT_ID",
  "archive_id": "ARCHIVE_ID"
}
```

Use `archives_get` for download metadata. `archives_restore` creates isolated
restored files; it does not replace the live run. `archives_pin` protects an archive
from retention cleanup. Imports use the private inbox and verified SHA-256, preserving
origin instead of forging a new run.

## Encrypt a local archive

```sh
preflight archive keygen /secure/backup.identity
preflight archive seal run.tar.zst run.tar.zst.age age1PUBLIC_RECIPIENT
preflight archive unseal run.tar.zst.age restored.tar.zst CIPHERTEXT_SHA256 /secure/backup.identity
```

Keep the identity file private and outside public packages. The recipient is public.
A checksum proves byte identity, not publisher authenticity. Archive expiry requires
a separate preview and rechecks readers/pins; run history is retained.
