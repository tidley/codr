import ast
import hashlib
from pathlib import Path
import unittest


class LegacyContractTests(unittest.TestCase):
    def test_frozen_090_control_flow_matches_the_published_source(self):
        text=(Path(__file__).parent/"fixtures/updater_090.py").read_text()
        expected={"probe":"c5c7573cf6a592e828a4c0f0c3e241c46e0896f89a6efb9b492082f2ec6f8733",
                  "perform":"4d3ae4035ad9932f433fe0f34397e6d35a1ab2626b6b18ab132c522c4dcfe356"}
        for name,digest in expected.items():
            node=next(n for n in ast.parse(text).body if isinstance(n,ast.FunctionDef) and n.name==name)
            self.assertEqual(hashlib.sha256(ast.get_source_segment(text,node).encode()).hexdigest(),digest)
