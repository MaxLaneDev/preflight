"""Generate a formula from checksum-verified public release assets; never pushes Git."""
import argparse
import json
import pathlib
import re
import release


def generate(assets, tag, destination):
    release.require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag), "stable release tag required")
    raw = (assets / "SHA256SUMS").read_bytes()
    release.require(len(raw) <= 1024 * 1024, "checksum list exceeds 1 MiB")
    checksums = {}
    for line in raw.decode("utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        release.require(match and match[2] not in checksums, "invalid or duplicate checksum entry")
        checksums[match[2]] = match[1]
    name = "preflight-" + tag[1:] + "-macos-aarch64.tar.gz"
    release.require(name in checksums, "macOS ARM64 checksum missing")
    package = assets / name
    manifest = release.verify(package, checksums[name])
    release.require(manifest["metadata"]["version"] == tag[1:], "package version differs from release tag")
    return release.formula(package, checksums[name],
                           "https://github.com/MaxLaneDev/preflight/releases/download/" + tag + "/" + name,
                           "https://github.com/MaxLaneDev/preflight", destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("assets", type=pathlib.Path)
    parser.add_argument("tag")
    parser.add_argument("destination", type=pathlib.Path)
    args = parser.parse_args()
    print(json.dumps(generate(args.assets, args.tag, args.destination)))
