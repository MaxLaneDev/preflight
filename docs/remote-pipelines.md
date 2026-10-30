# Run a stage remotely

```yaml
version: 1
pipeline:
  stages:
    - id: build
      worker: {os: linux, arch: aarch64, worker_id: WORKER_ID}
      command: {program: make, args: [build]}
      outputs:
        app: {path: build/app, type: executable}
```

Select a registered worker from this project. OS/architecture identify the
execution host, not the compiler target. Unavailable workers fail explicitly.

Inputs and declared outputs are digest-verified transfers. Upstream artifacts and
supported modules are materialized into the worker workspace. Logs stream while
the command runs. Native commands keep worker-user permissions.

Cancellation reaches the worker through lease renewal. Lost leases fence late
writes; interrupted commands are not replayed. A copied result is published only
after ordinary output/report validation. [SSH](worker-bootstrap.md) · [Docker](remote-containers.md).
