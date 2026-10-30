# Evidence and diagnostics (protocol 1.54+)

Start with build_report using the retained run ID. Its diagnostics field is a cached
observation; indexed=false or complete=false means unknown coverage, not zero errors.
Use diagnostics_report with project_id/run_id and optional baseline_id. Retain IDs,
offset/next_offset and progress. Continue within scan_bytes/scan_events budgets until
drained when the question needs full coverage; do not loop after drained+omitted.
No comparison is proof of a source fix. Missing/unexecuted stages cannot prove fixed.

Use evidence_search for literal text and project/run/batch/stage, exact prepared Git
SHA, severity/file/test and from_ms inclusive/to_ms exclusive filters. Severity/file
requires derived index coverage; inspect diagnostic_filter_complete. Project-wide
indexing advances asynchronously. Keep evidence IDs such as log:123.

Use evidence_read for the selected ID and bounded neighbors. Large events support
UTF-8 byte offset/max_bytes with neighbors=0; retain next_offset. The budget covers
the entire JSON response. Offsets refer to redacted text. Stop when enough evidence
is obtained and state any remaining coverage limit.

Use runs_compare with project_id/run_id/baseline_id for ordinary or experiment runs;
retain the returned pagination offset. Detailed source identities/diffs remain in
runs_compare_inputs and runs_source_diff. Friendly UI selectors do not change MCP's
explicit ID contract. No binary artifact content is embedded in resource replies.

Context levels: build_report (compact), diagnostics_report (diagnostic),
evidence_read/runs_source_diff/fixtures_diff (detailed). Use the smallest sufficient
level. Optional semantic/vector retrieval is not advertised.
