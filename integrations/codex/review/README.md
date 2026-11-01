# Review fixtures

Requires Git, jq and a C compiler (`cc`). Run on the package's native platform:

```sh
sh review/setup.sh /absolute/new/review-directory 49170
preflight --config /absolute/new/review-directory/server.toml serve
```

Use server 0.1.2 or newer. Configure the development MCP connection with the
bundled relay and `--server http://127.0.0.1:49170/mcp`. Do not replace an active
production connection. The setup command only creates fresh sample inputs; it
does not start services, register workspaces or change an existing repository.

Read `inputs.json`. Register its `project_root` as **Preflight review** and save
`pipeline.json` as **checks**, retaining the returned project ID and revision.
Run the base and each patch against the recorded Git base commit. Patches A/B
print `review-a`/`review-b`; the compiler-error patch genuinely fails compilation
with `preflight_review_unknown_symbol`. Retain those run IDs as review context.

For queue review, use the two independent A/B patches and `max_parallel=2`.
For cancellation, start two copies of `cancellation-pipeline.json` in this
dedicated server, supply their IDs, and select exactly one to cancel. The other
run must be allowed to finish. For source comparison, supply the A/B run IDs and
request `main.c` with the correct baseline direction.

The manifest contains five positive and three negative prompts. Run them in the
intended client and record actual outcomes separately. These fixtures and this
walkthrough are not a recorded demo or proof that the review prompts passed.
