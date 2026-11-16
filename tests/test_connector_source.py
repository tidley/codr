import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("connector_source", Path(__file__).resolve().parents[1] / "scripts/connector_source.py")
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class ConnectorSourceTests(unittest.TestCase):
    def test_release_pin_matches_mdk_017_cohort(self):
        settings = source.tomllib.loads((source.ROOT / "connector.toml").read_text())
        self.assertEqual(settings["version"], "0.17.0")
        self.assertEqual(settings["revision"], "4df811aea99269b437a6023be9446a5141e93cbe")
        self.assertEqual(settings["storage_cohort"], "mdk-0.17")
        toolchain = source.tomllib.loads((source.ROOT / "rust-toolchain.toml").read_text())
        self.assertIn("llvm-tools-preview", toolchain["toolchain"]["components"])
        workspace = source.tomllib.loads((source.ROOT / "vendor/mdk/Cargo.toml").read_text())
        self.assertEqual(workspace["workspace"]["package"]["version"], settings["version"])
        for name, dependency in workspace["workspace"]["dependencies"].items():
            if isinstance(dependency, dict) and dependency.get("git") == settings["repository"]:
                self.assertEqual(dependency["rev"], settings["revision"], name)

    def test_unexpected_revision_is_rejected_before_any_patch_or_write(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(source.subprocess, "check_output", return_value="unexpected\n"), patch.object(source.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "pinned revision"):
                source.prepare(directory)
            run.assert_not_called()
            self.assertFalse((Path(directory) / "CODR-CONNECTOR.json").exists())

    def test_existing_source_changes_are_preserved(self):
        settings = source.tomllib.loads((source.ROOT / "connector.toml").read_text())
        with tempfile.TemporaryDirectory() as directory, patch.object(source.subprocess, "check_output", side_effect=[settings["revision"] + "\n", " M local-work.rs\n"]), patch.object(source.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "local changes"):
                source.prepare(directory)
            run.assert_not_called()
            self.assertFalse((Path(directory) / "CODR-CONNECTOR.json").exists())


if __name__ == "__main__":
    unittest.main()
