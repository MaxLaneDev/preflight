# Codex plugin

The plugin connects to a running server at `http://127.0.0.1:4170/mcp`.
Install/start the [server](server-setup.md) first. The plugin does not include it.

## Build a local marketplace

```sh
sh scripts/package-codex-plugin.sh output/preflight-plugin
codex plugin marketplace add "$PWD/output/preflight-plugin" --json
codex plugin add preflight@preflight-local --json
```

Use a new destination for each package. Keep that directory available. Do not also
configure the same MCP endpoint as a standalone connection.

## Make a distributable ZIP

```sh
sh scripts/package-codex-plugin.sh output/preflight-plugin.zip --zip
sh scripts/test-codex-plugin.sh
```

Optional native hooks use the installed Preflight binary and shell scripts.
Review/trust each hook in Codex's hooks UI.
Installation never grants trust. SessionStart adds a brief workflow reminder;
UserPromptSubmit validates completion messages. Neither approves tools or starts builds.
[Completion delivery](codex-completion-hook.md) is separate.

Public directory submission is pending. OpenAI does not accept lifecycle hooks
in submitted ZIPs and requires eligibility for local MCP plugins.
[Official requirements](https://developers.openai.com/plugins/deploy/submission).
