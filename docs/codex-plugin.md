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

## Build a hook-free stdio package (0.1.2+)

Use a server archive from the same version as the packaging binary:

```sh
preflight package plugin-stdio output/preflight-0.1.2-macos-aarch64.tar.gz \
  --sha256 "$(awk 'NR==1 {print $1}' output/preflight-0.1.2-macos-aarch64.tar.gz.sha256)" \
  --repository "$PWD" --destination output/preflight-stdio-macos-aarch64.zip
```

The ZIP includes the native binary, licenses, icon, skill, portable `plugin.json`
and `mcp.json`. Its launch command uses the bundled binary, not a binary found on
`PATH`. It connects to an already running server. No Node, Python or lifecycle
hooks are required. The server's database and credentials stay outside the ZIP.
Choose this package or the HTTP plugin above; do not enable both connections.

Each archive targets one platform. The 0.1.2 candidate supports macOS ARM64/x86_64,
Linux ARM64/x86_64 and Windows x86_64. Use a matching archive; Windows uses
`./bin/preflight.exe`. The input checksum and package contents
are verified before writing the ZIP. Existing destinations are never replaced.

This command builds a package; it does not submit or publish it. OpenAI approval
for local MCP and a clean installation check in the target desktop client are
still required. A successful CLI relay test alone is not directory acceptance.

Listing metadata and five positive/three negative review cases are maintained in
`integrations/codex/submission.json`. The ZIP contains isolated C review fixtures;
see its `review/README.md`. A recorded demo and review-prompt outcomes are still
required before directory submission. The demo is currently deferred.
