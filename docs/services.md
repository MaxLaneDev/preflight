# Autostart

On macOS, use **Settings → Start Preflight at login**. It applies at the next login
and does not stop the current process. No browser is opened.

For explicit service files:

```sh
preflight --config /path/to/preflight.toml service export launchd /new/service-export
preflight --config /path/to/preflight.toml service export systemd /new/service-export
```

Open the generated README for exact install/status/remove commands. Export does
not activate anything and refuses existing output directories. Stop a manually
started instance before activating the same config as a service.

Service definitions use absolute paths and the current user's permissions. They
restart the server after failure. Services do not inherit your interactive shell;
configure tool paths and required environment explicitly. Credentials belong in
private files, not service definitions.

Cron/Windows dispatch exports invoke `run-due`; they are not a running MCP server.
The server already dispatches schedules internally while it runs.
