# Workers

Use [SSH bootstrap](worker-bootstrap.md) to install and connect a worker.

`workers_list`

<!-- mcp: workers_list -->
```json
{
  "project_id": "PROJECT_ID"
}
```

Choose a listed worker in the pipeline editor, or add a stage selector:

```yaml
worker:
  os: linux
  arch: aarch64
  worker_id: WORKER_ID
command: {program: make, args: [test]}
```

Omit `worker_id` for a matching OS/architecture pool. Omit `worker` for local work.
No match fails explicitly; work does not silently fall back to the server.

Workers are project-scoped and process one job at a time. Enrollment credentials
are private. SSH carries the loopback gateway; native public TLS is not provided.
[Job lifecycle](worker-jobs.md) · [Containers](remote-containers.md).
