"""Exercise Cargo's actual build-script cache, using a tiny native Rust fixture."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import install
import package_evidence


class BuildIdentityTests(unittest.TestCase):
    def test_dirty_rebuild_cannot_be_packaged_after_source_is_restored(self):
        with tempfile.TemporaryDirectory(prefix="codr-build-identity-") as temporary:
            base = Path(temporary)
            root = base / "source"
            (root / "src").mkdir(parents=True)
            (root / "Cargo.toml").write_text('[package]\nname="codr-identity-fixture"\nversion="0.13.0-preview.1"\nedition="2021"\n')
            shutil.copyfile(ROOT / "build.rs", root / "build.rs")
            clean = 'fn payload() -> &\'static str { "clean" }\n'
            (root / "src/payload.rs").write_text(clean)
            (root / "src/main.rs").write_text('''include!("payload.rs");
fn main() {
    println!("{{\\\"version\\\":\\\"0.13.0-preview.1\\\",\\\"source_commit\\\":\\\"{}\\\",\\\"source_dirty\\\":{},\\\"target\\\":\\\"{}\\\",\\\"payload\\\":\\\"{}\\\"}}",
        env!("CODR_SOURCE_COMMIT"), env!("CODR_SOURCE_DIRTY"), env!("CODR_BUILD_TARGET"), payload());
}
''')
            def run(*args):
                return subprocess.run(args, cwd=root, check=True, capture_output=True, text=True, timeout=180)
            run("git", "init", "-q")
            run("cargo", "generate-lockfile", "--offline")
            run("git", "add", "Cargo.toml", "Cargo.lock", "build.rs", "src")
            run("git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
            def build():
                output = run("cargo", "build", "--offline", "--quiet", "--message-format=json").stdout
                records = [json.loads(line) for line in output.splitlines()]
                # Cargo's artifact receipt reflects central output redirection;
                # a default target path or metadata directory need not do so.
                return next(Path(row["executable"]) for row in records
                            if row.get("reason") == "compiler-artifact" and row.get("executable"))
            binary = build()
            self.assertFalse(json.loads(run(str(binary)).stdout)["source_dirty"])
            (root / "src/payload.rs").write_text('fn payload() -> &\'static str { "modified-source-artifact" }\n')
            binary = build()
            retained = base / "retained-artifact"
            shutil.copy2(binary, retained)
            dirty = json.loads(run(str(retained)).stdout)
            self.assertEqual(dirty["payload"], "modified-source-artifact")
            self.assertTrue(dirty["source_dirty"], "Cargo reused a falsely clean source identity")
            # Restore source WITHOUT rebuilding. The executable must still carry
            # its dirty-build provenance even though packaging sees clean Git.
            (root / "src/payload.rs").write_text(clean)
            run("git", "diff", "--quiet", "HEAD", "--")
            with patch.object(install, "version", return_value="0.13.0-preview.1"), patch.dict(install.TARGETS, {install.platform_name(): dirty["target"]}):
                with self.assertRaisesRegex(install.InstallError, "identity differs"):
                    package_evidence.package(root, retained, base / "must-not-exist.tar.gz")
            self.assertFalse((base / "must-not-exist.tar.gz").exists())
