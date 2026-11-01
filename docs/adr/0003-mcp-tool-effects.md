# Declare MCP tool effects

Status: accepted, 2026-11-02.

Submission review requires explicit `readOnlyHint`, `destructiveHint` and
`openWorldHint` booleans. Classify every registered tool in one table applied
when constructing its router; a coverage test rejects unclassified additions.
Unknown tools retain conservative hints until classified.

Hints cover the maximum effects supported by the tool's schema. Mixed settings
read/write calls use write hints. Cleanup previews persist plans and are writes;
schedule timestamp previews are reads. Native commands and network/provider
operations use open-world hints. Read hints concern user-visible data, not
ephemeral decryption buffers, leases or derived search indexes.

Annotations never grant access or approve an action. Existing authorization,
scope and reference/lease checks remain authoritative. This adds standard MCP
discovery metadata without changing Preflight request/result schemas, cache key
formats or its protocol 1.72.0.
