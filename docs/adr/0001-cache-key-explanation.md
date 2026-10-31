# Explain source provenance separately from stage inputs

Status: accepted, 2026-10-31.

Different ordered patches can produce identical captured files. Stage evidence
then matches, but the existing build-cache key differs because it also includes
source provenance. Comparing stage evidence alone cannot explain this miss.

Protocol 1.72 adds build-cache identity and component comparisons to
`runs_stage_key`, using the same key function as execution. The cache format and
existing keys remain unchanged. Only hashes are exposed; patch text and private
source paths are not copied into the response. A diagnostic key never authorizes
reuse. Actual decisions remain available through `runs_cache`.

No database migration or Web UI change is required. Older clients ignore these
additive fields; older stored runs without stage evidence return null.
