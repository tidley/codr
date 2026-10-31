from pathlib import Path
import unittest


class CombinedPackagingTests(unittest.TestCase):
    def test_compatibility_connector_is_verified_but_not_a_default_release_asset(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        full = workflow.split("  binaries:\n",1)[1].split("  linux-preview:\n",1)[0]
        self.assertIn("cargo test --locked -p agent-control -p agent-connector",full)
        self.assertIn("just fast-ci",full)
        package = full.split("- name: Package dedicated connector",1)[1].split("- uses: actions/upload-artifact",1)[0]
        self.assertIn("connector-checks/$asset",package)
        self.assertNotIn('"dist/$asset"',package)
        preview = workflow.split("- name: Verify complete Linux assets",1)[1]
        self.assertNotIn("for name in ('codr', 'codr-wn-agent')",preview)

    def test_both_worker_packaging_paths_include_embedded_dependency_licenses(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        self.assertEqual(workflow.count('-C "$GITHUB_WORKSPACE/vendor/mdk" LICENSE README.codr.md'), 2)
        self.assertEqual(workflow.count('-C "$GITHUB_WORKSPACE/vendor/mdk/vendor/hpke-rs-libcrux" LICENSE-MPL-2.0'), 2)
