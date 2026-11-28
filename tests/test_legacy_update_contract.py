import ast
import hashlib
from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest.mock import patch


class LegacyContractTests(unittest.TestCase):
    def test_frozen_090_control_flow_matches_the_published_source(self):
        text=(Path(__file__).parent/"fixtures/updater_090.py").read_text()
        expected={"probe":"c5c7573cf6a592e828a4c0f0c3e241c46e0896f89a6efb9b492082f2ec6f8733",
                  "perform":"4d3ae4035ad9932f433fe0f34397e6d35a1ab2626b6b18ab132c522c4dcfe356"}
        for name,digest in expected.items():
            node=next(n for n in ast.parse(text).body if isinstance(n,ast.FunctionDef) and n.name==name)
            self.assertEqual(hashlib.sha256(ast.get_source_segment(text,node).encode()).hexdigest(),digest)

    def test_cohort_fixture_checks_both_stable_and_preview_refusal(self):
        fixtures = Path(__file__).parent / "fixtures"
        # No runtime, accounts, network or service manager: only the wrapper's
        # branch expectations are tested here. Rust's integration test supplies
        # the real executable and its update-info metadata separately.
        with patch.object(sys, "path", [str(fixtures), str(fixtures.parents[1] / "scripts"), *sys.path]):
            import updater_090 as legacy
            for version in ("0.13.0", "0.13.0-preview.1"):
                with self.subTest(version=version), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    binary = root / "fixture-binary"
                    binary.write_bytes(b"never executed")
                    metadata = {"version": version, "state_version": 1,
                                "connector": {"version": "0.19.0"}}
                    with patch.object(legacy, "probe", return_value=metadata), patch.object(sys, "argv", ["check_090_upgrade.py", str(root), str(binary)]):
                        runpy.run_path(str(fixtures / "check_090_upgrade.py"), run_name="__main__")
