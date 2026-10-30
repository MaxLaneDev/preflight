# Global search

Use the top-bar search field or **⌘/Ctrl K**. Search for a name, UUID, commit/hash,
pipeline, run or test. Results are grouped by object type.

`search_global`

<!-- mcp: search_global -->
```json
{
  "query": "build",
  "project_id": "PROJECT_ID",
  "limit": 10
}
```

Omit `project_id` to search all permitted projects. Pass returned `next_cursor` as
`after` for more results. [Evidence search](evidence.md) searches log contents.
