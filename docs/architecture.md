# Architecture

```text
MCP ─┐
HTTP ├─ Engine ─ scheduler ─ native process / Docker / worker
Web ─┘     │
        SQLite + files
```

| Object | Meaning |
| --- | --- |
| Project | Registered source root within the operator allowlist |
| Pipeline | Named immutable configuration revisions |
| Run | One captured source/configuration and its results |
| Stage | A command with declared dependencies, inputs and outputs |
| Experiment | Saved comparison settings and separate run history |
| Module | Verified published artifact with compatibility/retention references |

MCP and HTTP share engine operations and validators. The Web UI is bundled;
`preflight-client` provides a compatibility listener. No external service is required.

SQLite stores identity, status, ownership, plans and durable events. Payloads live
under the data directory. Leases protect active operations; uncertain state fails
closed until recovery. Source copies isolate accidental mutation, not malicious
native commands.

MCP transport version, Preflight tool contract and database migration versions are
independent. Migrations are contiguous, checksummed and transactional; future
schemas are rejected. [Recovery](operation-recovery.md) · [Backup](server-backup.md).
