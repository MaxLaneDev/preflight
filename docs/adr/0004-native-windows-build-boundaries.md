# Native Windows build boundaries

Windows CI exposed two shared defects: a cleared build environment hid MSVC
discovery directories, causing Rust to invoke GNU `link.exe`, and filesystem
relative paths carried backslashes into portable manifests.

The Windows executor preserves standard installation directories and compiler
library/SDK discovery variables. It still clears unrelated variables. The same
allowlist feeds stage-key and worker-receipt hashes, so environment changes
invalidate reuse; plaintext environment values are not written to these records.

Internally derived `Path` values serialize component by component with `/`.
Wire inputs still reject backslashes, drive prefixes and traversal. Package
licenses, plugin skills and imported CI artifacts use this boundary. Native
executable paths remain native paths, including in command arguments.

Runtime-policy writes use the worker's existing `StateLock`, which explicitly
unlocks on drop. Closing a file alone can leave a fork-inherited lock alive.

Git receives ordinary Windows drive/UNC paths with long-path support enabled;
internal filesystem checks retain canonical paths. ZIP entries explicitly use
Unix permission metadata on every producer OS. Tracked text uses LF so bundle
and lockfile checksums do not depend on the checkout host.

Windows worker output bundles preserve explicitly declared executable types in
the SHA-covered manifest; Windows has no Unix execute bit to recover from a file.
The server still validates declared paths, types and transferred content.
Stored worker outputs retain the verified transfer bundle, including its hashed
execution metadata, and decode it again before collection. Versioned private
bundle directories coexist with legacy stored trees; both formats verify their
recorded digest. No database migration or wire protocol change is needed.

`setup` retains its POSIX contract. Windows controllers can dispatch it to Linux
workers or containers; native Windows targets reject it. Shared artifact, test
and cancellation checks still run on Windows with native executable fixtures.

Acceptance requires native Windows CI, package installation and Web/MCP startup.
A successful macOS check does not prove Windows acceptance.
