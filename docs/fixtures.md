# Fixture batches

Define fixtures inside the project, then run:

```sh
preflight --config /path/to/preflight.toml fixtures run /path/to/project
preflight --config /path/to/preflight.toml status RUN_ID
```

`.preflight/fixtures.yaml` declares `version`, a build pipeline, a `compiler`
artifact reference, `adapter`, `suites` and optional `max_parallel_groups`.
The adapter defines how to compile/run cases; each suite defines files, execution
mode and expectations. Read the `fixtures_validate` input schema for the current case structure.

| Comparator | Example |
| --- | --- |
| Exact output | `stdout: {exact: "ok\n"}` |
| Ignore outer whitespace | `stdout: {trim: "ok"}` |
| Substring | `stderr: {contains: "syntax error"}` |
| Structured value | `stdout: {json: {ok: true}}` |
| Numeric tolerance | `stdout: {numeric: {value: 3.14, tolerance: 0.01}}` |
| Baseline file | `stdout: {snapshot: expected.txt}` |

Groups can run in parallel; cases within a group are sequential. Truncation and
missing baselines are failures. Results retain expected/actual checks and diffs.
`fixtures_results` pages cases; `fixtures_diff` reads one case's comparison.
