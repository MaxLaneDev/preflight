# Guides

[Set up the server](server-setup.md) first. All commands and requests use placeholder
paths/IDs; replace them with your own paths and returned IDs.

| Task | Read |
| --- | --- |
| Install, configure, autostart | [Setup](server-setup.md), [settings](server-settings.md), [services](services.md) |
| Connect a model | [MCP](mcp-configuration.md), [Codex plugin](codex-plugin.md), [completion](completion-events.md) |
| Build and test | [Pipelines](pipelines.md), [editor](pipeline-editor.md), [test reports](test-reports.md), [fixtures](fixtures.md) |
| Compare or plan changes | [Experiments](experiments.md), [queues](change-queues.md), [schedules](schedules.md) |
| Select sources | [Git/dirty](git-sources.md), [multiple repositories](multi-sources.md) |
| Prepare compilers and reuse outputs | [Setup](environment-setup.md), [profiles](toolchain-profiles.md), [pins](pinned-toolchains.md), [modules](prepared-modules.md), [cache](build-cache.md) |
| Connect workers | [SSH](worker-bootstrap.md), [workers](workers.md), [remote stages](remote-pipelines.md), [containers](remote-containers.md) |
| Inspect results | [Web UI](client.md), [execution](execution-overview.md), [search](evidence.md), [logs](log-storage.md) |
| Reclaim disk space | [Usage](workspace-maintenance.md), [cleanup](storage-gc.md), [policies](storage-policies.md), [module pins](module-retention.md) |
| Keep or recover data | [Run archives](run-archives.md), [server backup](server-backup.md), [abandoned operations](operation-recovery.md) |
| Import external results | [External CI](external-ci-setup.md) |
| Develop or distribute | [Architecture](architecture.md), [status](status.md), [releases](releases.md), [license](licensing.md) |

MCP examples name the tool above its JSON arguments. Discover schemas through the
server when a field or supported feature is unclear. 
