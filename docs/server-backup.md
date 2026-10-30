# Back up the server

Wait for work to finish and stop the server before offline backup/restore.

```sh
preflight backup create /path/to/preflight.toml /backups/server.tar.zst
preflight backup verify /backups/server.tar.zst SHA256_FROM_CREATE
preflight backup restore /backups/server.tar.zst SHA256_FROM_CREATE /recovery/new-directory
```

Restore into a new directory. Keep configuration/credentials private; a backup can
contain project sources and logs. Encrypt it with the [archive CLI](run-archives.md).

Before switching an installation:

```sh
preflight backup activation-preview /recovery/new-directory /path/to/preflight.toml
preflight backup activate /recovery/new-directory /path/to/preflight.toml PLAN_ID
```

Review the plan. Activation must pass ownership, path, schema and quiescence checks;
it never merges arbitrary live databases. Binary rollback cannot undo a forward
schema migration. Keep the previous config/binary with its matching backup.
