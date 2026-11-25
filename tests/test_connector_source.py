import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("connector_source", Path(__file__).resolve().parents[1] / "scripts/connector_source.py")
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class ConnectorSourceTests(unittest.TestCase):
    def test_release_pin_matches_mdk_019_cohort(self):
        settings = source.tomllib.loads((source.ROOT / "connector.toml").read_text())
        self.assertEqual(settings["version"], "0.19.0")
        self.assertEqual(settings["revision"], "3fce95465cdea1edc8456dbd1c6691d1c8dc1f2e")
        self.assertEqual(settings["storage_cohort"], "mdk-0.19")
        toolchain = source.tomllib.loads((source.ROOT / "rust-toolchain.toml").read_text())
        self.assertIn("llvm-tools-preview", toolchain["toolchain"]["components"])
        workspace = source.tomllib.loads((source.ROOT / "vendor/mdk/Cargo.toml").read_text())
        self.assertEqual(workspace["workspace"]["package"]["version"], settings["version"])
        for name, dependency in workspace["workspace"]["dependencies"].items():
            if isinstance(dependency, dict) and dependency.get("git") == settings["repository"]:
                self.assertEqual(dependency["rev"], settings["revision"], name)
        root = source.tomllib.loads((source.ROOT / "Cargo.toml").read_text())
        self.assertEqual(root["patch"], workspace["patch"])
        self.assertEqual(root["patch"]["crates-io"]["libsqlite3-sys"]["rev"],
                         "5ae7fdf83085c595fd54f977b3a56ccacabaf16b")
        self.assertEqual(root["patch"]["crates-io"]["hax-lib-macros"]["rev"],
                         "8f9cb576e58f6cc7e9ed249a7d4439b0f7ed0da7")

    def test_lockfile_keeps_url_security_fix_and_removes_retired_preview_crates(self):
        packages = source.tomllib.loads((source.ROOT / "Cargo.lock").read_text())["package"]
        settings = source.tomllib.loads((source.ROOT / "connector.toml").read_text())
        for package in packages:
            if package.get("source", "").startswith("git+" + settings["repository"]):
                self.assertEqual(package["version"], settings["version"])
                self.assertEqual(package["source"], "git+" + settings["repository"] +
                                 "?rev=" + settings["revision"] + "#" + settings["revision"])
        urls = [p for p in packages if p["name"] == "url"]
        self.assertTrue(urls)
        for package in urls:
            self.assertGreaterEqual(tuple(map(int, package["version"].split("."))), (2, 5, 9))
        retired = {"agent-stream-compose", "transport-quic-stream", "transport-quic-broker"}
        self.assertFalse(retired & {p["name"] for p in packages})

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
