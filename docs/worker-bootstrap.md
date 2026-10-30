# Connect a worker over SSH

## Server configuration

```toml
worker_bind = "127.0.0.1:4171"
ssh_command = "/usr/bin/ssh"
```

Adding the listener requires a restart after active work finishes. Configure SSH
keys and trusted `known_hosts`; Preflight rejects unknown host keys and does not forward your agent.

## Prepare a package

On the matching worker OS/architecture, using its built Preflight binary:

```sh
preflight worker package /path/to/bundles/linux-aarch64
```

Copy the bundle under an allowed server directory. The package records its actual
platform; renaming a macOS binary does not make it a Linux binary.

## Install and connect with one command

Save `connection.json` on the server computer:

```json
{
  "project_id":"PROJECT_ID",
  "name":"linux-worker",
  "host":"user@worker.local",
  "bundle_dir":"/absolute/path/to/bundles",
  "port":22,
  "remote_port":4171
}
```

```sh
preflight --config /path/to/preflight.toml worker-ssh connection.json
```

The command probes the host, chooses a bundle, installs/enrolls the worker and
opens a reverse loopback tunnel. It stays in the foreground; `once: true` is only
a connection check and exits. Keep the project/name and installation root stable
to reuse worker identity. No remote compiler, Docker or sudo is installed.

## Managed MCP connection

`worker_ssh_start`

<!-- mcp: worker_ssh_start -->
```json
{
  "request_id": "connect-1",
  "connection": {
    "project_id": "PROJECT_ID",
    "name": "linux-worker",
    "host": "user@worker.local",
    "bundle_dir": "/absolute/path/to/bundles"
  },
  "reconnect": {
    "max_retries": 3,
    "delay_seconds": 10
  }
}
```

`worker_ssh_status`

<!-- mcp: worker_ssh_status -->
```json
{
  "project_id": "PROJECT_ID",
  "connection_id": "CONNECTION_ID"
}
```

Use `worker_ssh_stop` to stop this connection and its retry timer. Restart does
not silently relaunch interrupted connections. [Prepare tools](environment-setup.md).
