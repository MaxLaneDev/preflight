# Pipelines

Save `.preflight/pipeline.yaml`, or send the same structure as an MCP definition.

```yaml
version: 1
pipeline:
  max_parallel_stages: 2
  stages:
    - id: build
      command:
        program: cc
        args: [main.c, -o, app]
      outputs:
        app: {path: app, type: executable}
    - id: smoke
      kind: test
      needs: [build]
      command:
        program: '${artifact.build.app}'
```

The smoke stage checks the process exit code. Named test results require a
[report](test-reports.md).

| Field | Use |
| --- | --- |
| `needs` | Dependencies; independent branches can run together |
| `cwd` | Directory relative to the stage workspace |
| `env` | Explicit environment variables |
| `inputs` | Destination path → upstream artifact/module reference |
| `outputs` | Named file, executable or directory artifacts |
| `junit` | Exact report paths relative to `cwd` |
| `resources` | Scheduling slots and optional memory reservation |
| `setup` | Commands before the main command |
| `worker` / `container` | [Remote](remote-pipelines.md) or [container](remote-containers.md) execution |

```yaml
resources: {slots: 2, memory_mib: 2048}
command:
  program: sh
  args: [-c, 'make -j2 test']
  timeout_seconds: 600
```

A memory reservation requires the server memory budget to be enabled.
Commands use literal argv; use a shell explicitly for shell syntax.

Each stage gets a separate source copy. Pass declared outputs through whole
references such as `${artifact.build.app}`. Use relative paths without `..` or
symlinks. `.preflight/` is reserved for configuration and materialized inputs.

A failed dependency blocks its descendants. Cancel stops active process trees and
pending stages. Completed results remain in history.

Limits: 64 KiB definition, 32 stages, 1–32 parallel stages. Discover the current
contract with `pipeline_schema`; check a definition with `pipeline_validate`.
See [build cache](build-cache.md), [setup](environment-setup.md) and [retries](retries.md).
