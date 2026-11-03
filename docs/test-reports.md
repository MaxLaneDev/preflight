# Test results

A test stage can represent unit, integration, functional, fuzz or fixture testing.
Commands choose the runner; reports provide the common per-test display.

```yaml
id: checks
kind: test
command: {program: ./run-tests.sh}
testing:
  kind: integration
  reports:
    - {format: preflight_json, path: results.json}
```

`results.json`:

```json
{"version":1,"cases":[{"suite":"parser","name":"roundtrip","status":"passed","file":"tests/parser.c","line":12,"duration_seconds":0.03}]}
```

For JUnit:

```yaml
kind: test
command: {program: ./run-tests.sh}
junit: [report.xml]
```

Declared report paths must be created by this run. Missing/malformed reports fail;
assertion failures override exit zero. No report means unknown counts, not zero.
The UI tree uses each run's own statuses; file/line evidence enables source/VS Code.

## Require passing test evidence

Add to the pipeline's `pipeline` object:

```yaml
acceptance:
  - id: tests
    rule:
      kind: tests
      min_passed: 1
      max_failed: 0
      require_complete: true
```

Inspect the run's Acceptance tab. Incomplete/missing evidence does not satisfy
this criterion even when the process exits zero.

## Why is the Tests tab empty?

Preflight reads fixture results and declared JUnit or Preflight JSON reports.
It does not infer cases from command names, successful exits, or arbitrary console output.
Mark test commands `kind: test`, choose `testing.kind`, and configure the report path.
The command must generate that report. Reports belong to the recorded run; changing a
pipeline does not rewrite old results.

Each command has `timeout_seconds` (default 300, allowed 1–3600). This is an execution
deadline, not a queue wait limit or a limit on the sum of all stages. Timeout terminates
the process tree and records `timed_out`.

```json
{"id":"integration","kind":"test","command":{"program":"./run-tests.sh","timeout_seconds":1800},"testing":{"kind":"integration","reports":[{"format":"junit","path":"results.xml"}]}}
```
