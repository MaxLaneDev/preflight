# Relay stdio to the existing MCP backend

Status: accepted, 2026-11-02.

The desktop-only plugin submission path requires a local stdio MCP entry. Starting
another Preflight engine would contend for the live database lock and create a
second authority for runs and retention.

`preflight mcp-proxy` relays MCP frames between stdio and the existing loopback HTTP
endpoint. Tool schemas, results, errors and authorization remain backend-owned.
The relay opens no configuration or database. Requests can overlap; pending sends
are bounded at 64 and input frames use rmcp's 16 MiB cap. Overload fails explicitly.
Expiry never triggers automatic mutation replay. Transport failures report an
unknown outcome, requiring inspection before a retry.

HTTP proxies and redirects are disabled. Endpoints accept loopback IPs only,
without URL credentials or query strings. Optional bearer credentials come from
a named environment variable and use sensitive headers. Stdout carries MCP only.
Closing stdin closes the upstream session; it does not cancel submitted builds.

The initialized notification must be acknowledged by HTTP before subsequent
requests are forwarded. Backend failure exits even when the parent keeps stdin
open, avoiding Tokio's blocking-stdin shutdown wait.

Integration checks cover tool-schema parity, concurrent calls, cancellation,
credential forwarding, delayed initialization, expired-session non-replay and
failure with an open parent pipe.

`package plugin-stdio` builds a hook-free portable ZIP from a checksum-verified,
same-version server archive. It bundles the native binary and its licenses;
the client launches `${PLUGIN_ROOT}/bin/preflight mcp-proxy`. Each ZIP targets
one supported ARM64 platform. Credentials, configuration and lifecycle hooks are
excluded. Packaging does not grant directory eligibility or submit the plugin.
