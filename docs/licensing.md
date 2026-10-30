# License

Preflight uses the [MIT license](../LICENSE).

Release packages include `THIRD_PARTY_NOTICES.json` and dependency licenses.
The embedded editor's notices come from its pinned package lock and bundle digest.
Regenerate them after changing editor dependencies:

```sh
node scripts/client-notices.mjs
```

The release packager checks notice fingerprints before shipping. Dependencies
retain their own licenses; the project's MIT license does not replace them.
