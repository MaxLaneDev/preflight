"""Check the public source boundary and executable example files."""
import importlib.util
import pathlib
import tempfile
import unittest
import subprocess
import os

spec = importlib.util.spec_from_file_location("exporter", pathlib.Path(__file__).with_name("export-public-source.py"))
exporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(exporter)


class ExportTests(unittest.TestCase):
    def test_clean_source_keeps_compile_inputs_and_example_modes(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = pathlib.Path(temporary) / "source"
            exporter.export(destination)
            self.assertEqual({p.name for p in (destination / "examples").iterdir()}, {"c-basic", "cpp-basic", "go-basic"})
            for relative in ["examples/cpp-basic/build.sh", "examples/cpp-basic/check.sh"]:
                self.assertEqual((destination / relative).stat().st_mode & 0o777,
                                 (exporter.ROOT / relative).stat().st_mode & 0o777)
                if os.name != "nt":
                    self.assertTrue((destination / relative).stat().st_mode & 0o111)
            for name in [".git", ".validation", "data", "target", "preflight.toml", "PREFLIGHT_MASTER_PLAN.md"]:
                self.assertFalse((destination / name).exists(), name)
            self.assertEqual(len(list((destination / "migrations").glob("*.sql"))),
                             len(list((exporter.ROOT / "migrations").glob("*.sql"))))
            self.assertEqual((destination / "docs/status.md").read_bytes(),
                             (exporter.ROOT / "docs/status.md").read_bytes())
            with self.assertRaises(FileExistsError):
                exporter.export(destination)
            subprocess.run(["git", "init", "-q", str(destination)], check=True)
            subprocess.run(["git", "-C", str(destination), "add", "."], check=True)
            original = exporter.ROOT
            try:
                exporter.ROOT = destination
                second = pathlib.Path(temporary) / "second"
                exporter.export(second)
                self.assertEqual((second / "integrations/codex/skills/preflight-mcp/SKILL.md").read_bytes(),
                                 (destination / "skills/preflight-mcp/SKILL.md").read_bytes())
            finally:
                exporter.ROOT = original


if __name__ == "__main__":
    unittest.main()
