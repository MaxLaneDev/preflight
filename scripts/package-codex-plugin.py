"""Build a self-contained local marketplace; never copy server data or trust settings."""
import argparse
import json
from pathlib import Path
import shutil
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def package(destination):
    # Require a new directory: never overwrite a user's marketplace or installed plugin.
    destination.mkdir(parents=True, exist_ok=False)
    plugin = destination / "plugins/preflight"
    for relative in (".codex-plugin/plugin.json", ".mcp.json", "hooks/hooks.json", "hooks/preflight.py"):
        target = plugin / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / "integrations/codex" / relative, target)
    for relative in sorted(str(path.relative_to(ROOT / "skills/preflight-mcp")) for path in (ROOT / "skills/preflight-mcp").rglob("*.md")):
        target = plugin / "skills/preflight-mcp" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / "skills/preflight-mcp" / relative, target)
    icon = plugin / "assets/icon.png"
    icon.parent.mkdir(parents=True)
    shutil.copyfile(ROOT / "assets/branding/preflight-icon.png", icon)
    manifest = destination / ".agents/plugins/marketplace.json"
    manifest.parent.mkdir(parents=True)
    manifest.write_text(json.dumps({
        "name": "preflight-local",
        "interface": {"displayName": "Preflight Local"},
        "plugins": [{"name": "preflight", "source": {"source": "local", "path": "./plugins/preflight"},
                     "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                     "category": "Productivity"}]
    }, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="New marketplace directory")
    parser.add_argument('--zip', action='store_true', help='Create a plugin-root ZIP for manual distribution')
    args = parser.parse_args()
    if args.zip:
        with tempfile.TemporaryDirectory(prefix='preflight-plugin-package-') as temporary:
            marketplace = Path(temporary)/'marketplace'
            package(marketplace)
            plugin = marketplace/'plugins/preflight'
            with zipfile.ZipFile(args.destination, 'x', compression=zipfile.ZIP_DEFLATED) as archive:
                for file in sorted(plugin.rglob('*')):
                    if file.is_file(): archive.write(file, file.relative_to(plugin))
    else:
        package(args.destination)
    print(args.destination.resolve())
