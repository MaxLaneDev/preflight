# External CI

Open **Settings → External CI → Add source**. Choose GitHub/GitLab, the local
project, provider URL and repository/project identifier. Configure the token
through an environment-variable name; keep its value out of saved definitions.

Start the server with that variable in its environment. Grant only the provider
read permissions needed for runs, logs and artifacts.

`ci_sources_list`

<!-- mcp: ci_sources_list -->
```json
{
  "project_id": "PROJECT_ID"
}
```

`ci_sync`

<!-- mcp: ci_sync -->
```json
{
  "source": "SOURCE_NAME",
  "request_id": "import-1",
  "max_pages": 2,
  "max_runs": 1
}
```

Inspect `ci_sync_status`/`ci_runs_list`. Imports retain provider run/attempt IDs,
logs and parsed reports; retries resume recorded cursors. Explicit artifact fetch
verifies the selected object. Provider mutations, triggering remote CI, and
changing its pipelines are not supported. Empty sources make no network requests.
