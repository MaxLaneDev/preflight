# Containers on a worker

Enable Docker in the worker's connection request:

```json
{"runtime":{"docker_command":"/usr/bin/docker","max_memory_mib":2048}}
```

The operator installs Docker and grants daemon access. Jobs cannot replace this
executable or raise the worker memory limit.

```yaml
id: tests
worker: {os: linux, arch: aarch64, worker_id: WORKER_ID}
executor: docker
container:
  image: repository@sha256:IMAGE_DIGEST
  platform: linux/arm64
command: {program: make, args: [test]}
```

Use an existing immutable image/digest on that daemon; consumer stages do not
implicitly pull. `setup` runs inside the container. A producer's image-ID file can
be used through `container.image_from: '${artifact.prepare.image}'`; producer and
consumer must use the same explicit worker identity and daemon.

## Workspace and cleanup

Only the job workspace is mounted. Docker socket/host directories are not mounted.
Defaults include read-only root, no network, dropped capabilities, no-new-privileges,
a PID limit and private `/tmp`; the worker memory policy is enforced.

Success, failure, timeout and cancellation remove the owned job container and
transferred workspace after results return. Images are retained separately.
Uncertain cleanup fences the worker and keeps the workspace for recovery.

Inspect `runs_worker_jobs` and `worker_recovery_status` before recovery. Never attest
that processes or daemon create requests stopped without checking them. Remote
container cache reuse and prepared-module publication remain disabled.
