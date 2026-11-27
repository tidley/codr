import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
import install
import package_evidence

spec = importlib.util.spec_from_file_location("fresh_room_setup", SCRIPTS / "room_setup.py")
room = importlib.util.module_from_spec(spec)
spec.loader.exec_module(room)


class RoomOnboardingTests(unittest.TestCase):
    def setUp(self):
        # Release builds use committed source. The test checkout is intentionally
        # dirty while its source is under review; only this Git cleanliness probe
        # is substituted, never archive/header/digest/identity validation.
        run = package_evidence.subprocess.run
        def fixture_git(args, *positional, **keywords):
            if args[:1] == ["git"] and "--quiet" in args and "diff" in args:
                import subprocess
                return subprocess.CompletedProcess(args, 0)
            return run(args, *positional, **keywords)
        self.clean_source = patch.object(package_evidence.subprocess, "run", side_effect=fixture_git)
        self.clean_source.start()
        self.addCleanup(self.clean_source.stop)

    def release_info(self):
        return {"version": install.version(), "source_commit": package_evidence.evidence(SCRIPTS.parent)["source_commit"],
                "source_dirty": False, "target": install.TARGETS[install.platform_name()]}

    def executable_fixture(self, path):
        # Header-only fixture, never executed. Runtime identity probing is mocked
        # separately; platform/architecture inspection still reads actual bytes.
        host = install.platform_name()
        header = bytearray(64)
        if host.startswith("linux-"):
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<HH", header, 16, 2, 62 if host.endswith("x86_64") else 183)
        else:
            header[:4] = b"\xcf\xfa\xed\xfe"
            struct.pack_into("<I", header, 4, 0x01000007 if host.endswith("x86_64") else 0x0100000c)
            struct.pack_into("<I", header, 12, 2)
        path.write_bytes(header)
        path.chmod(0o755)

    def test_packager_refuses_non_executable_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            binary = base / "not-a-binary"
            binary.write_text("unrelated file")
            with self.assertRaises(install.InstallError):
                package_evidence.package(SCRIPTS.parent, binary, base / "bad.tar.gz")
            self.assertFalse((base / "bad.tar.gz").exists())

    def fixture(self, base):
        root, package, project = base / "room", base / "package", base / "project"
        root.mkdir(mode=0o700)
        package.mkdir()
        project.mkdir()
        account = "ab" * 32
        key = json.dumps(["herdr:opencode", "opencode", "id", "ses_fresh"])
        journal = {"phase": "bound", "options": {"root": str(root), "directory": str(project), "target_group": "aa"},
                   "key": key, "account": account, "native_session": "ses_fresh"}
        binding = {"sessionID": "ses_fresh", "key": key, "account": account, "group": "aa"}
        for name, data in (("setup.json", journal), ("room-input.json", binding)):
            path = root / name
            path.write_text(json.dumps(data))
            path.chmod(0o600)
        adapter = package / "adapters/opencode-room-input"
        adapter.mkdir(parents=True)
        files = {}
        for name in ("index.mjs", "admission.mjs", "transport.mjs", "package.json"):
            content = name.encode()
            (adapter / name).write_bytes(content)
            files[f"adapters/opencode-room-input/{name}"] = hashlib.sha256(content).hexdigest()
        (package / "CODR-SOURCE.json").write_text(json.dumps({"files": files}))
        (package / "herdr-white-noise-bridge").write_bytes(b"fixture executable, never run")
        return root, package, project

    def test_semver_accepts_preview_without_weakening_stable(self):
        for version in ("0.13.0-preview.1", "0.12.2", "1.0.0-rc.0"):
            self.assertTrue(install.valid_version(version), version)
        for version in ("01.2.3", "1.2.3-01", "1.2.3-", "../1.2.3", "1.2", "1.2.3-preview..1"):
            self.assertFalse(install.valid_version(version), version)
        with patch.object(install, "tomllib", None):
            self.assertEqual(install.version(), "0.13.0-preview.1")

    def test_foreground_preserves_settings_and_has_exact_combined_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, package, project = self.fixture(Path(temporary).resolve())
            original = {"mcp": {"private": {"enabled": True}}, "permission": {"bash": "ask"},
                        "plugins": [{"package": "existing", "options": {"keep": 1}}], "model": "unchanged"}
            (project / "opencode.json").write_text(json.dumps(original))
            result = room.configure(root, package, "foreground", system="darwin")
            settings = json.loads((project / "opencode.json").read_text())
            for field in ("mcp", "permission", "model"):
                self.assertEqual(settings[field], original[field])
            self.assertEqual(settings["plugins"][0], original["plugins"][0])
            self.assertEqual(settings["plugins"][1]["options"]["binding_file"], str(root / "room-input.json"))
            self.assertEqual(result["argv"], [str(package / "herdr-white-noise-bridge"), "--config", str(root / "config.toml"), "run", "--portr", "--messaging-home", str(root / "messaging")])
            self.assertFalse(result["service_started"])
            self.assertFalse((root / "room.plist").exists())
            with self.assertRaises((FileExistsError, room.SetupError)):
                room.configure(root, package, "foreground", system="darwin")
            self.assertEqual(json.loads((project / "opencode.json").read_text()), settings)

    def test_launchd_emits_only_new_dedicated_job_without_loading_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, package, project = self.fixture(Path(temporary).resolve())
            with patch("subprocess.run", side_effect=AssertionError("must not launch anything")):
                result = room.configure(root, package, "launchd", system="darwin")
            payload = plistlib.loads((root / "room.plist").read_bytes())
            self.assertEqual(payload["ProgramArguments"], result["argv"])
            self.assertEqual(payload["WorkingDirectory"], str(project))
            self.assertFalse(payload["KeepAlive"])
            self.assertTrue(payload["Label"].startswith("chat.codr.room."))
            self.assertEqual(result["launch_argv"], ["launchctl", "bootstrap", f"gui/{os.geteuid()}", str(root / "room.plist")])
            self.assertEqual((root / "room.plist").stat().st_mode & 0o777, 0o600)

    def test_project_sync_failure_retains_intent_without_completion_or_replay(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, package, project = self.fixture(Path(temporary).resolve())
            sync = os.fsync
            project_inode = project.stat().st_ino
            def fail_project_sync(fd):
                if os.fstat(fd).st_ino == project_inode:
                    raise OSError("fixture project directory sync failure")
                sync(fd)
            with patch.object(room.os, "fsync", side_effect=fail_project_sync):
                with self.assertRaises(OSError):
                    room.configure(root, package, "foreground", system="darwin")
            self.assertTrue((root / "adapter-install-intent.json").exists())
            self.assertFalse((root / "adapter-installed.json").exists())
            preserved = (project / "opencode.json").read_bytes()
            with self.assertRaises(room.SetupError):
                room.configure(root, package, "foreground", system="darwin")
            self.assertEqual((project / "opencode.json").read_bytes(), preserved)

    def test_refuses_linked_jsonc_conflicting_and_non_mac_configuration(self):
        for kind in ("linked", "jsonc", "dangling-jsonc", "fifo", "conflict", "linux"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root, package, project = self.fixture(Path(temporary).resolve())
                if kind == "linked":
                    (project / "opencode.json").symlink_to(root / "setup.json")
                elif kind == "jsonc":
                    (project / "opencode.jsonc").write_text("// preserve me")
                elif kind == "dangling-jsonc":
                    (project / "opencode.jsonc").symlink_to(project / "missing-config")
                elif kind == "fifo":
                    os.mkfifo(project / "opencode.json", mode=0o600)
                elif kind == "conflict":
                    (project / "opencode.json").write_text(json.dumps({"plugins": ["codr.room-input"]}))
                with self.assertRaises(room.SetupError):
                    room.configure(root, package, "foreground", system="linux" if kind == "linux" else "darwin")
                self.assertFalse((root / "adapter-install-intent.json").exists())

    def test_pending_binding_and_modified_adapter_are_refused(self):
        for kind in ("pending", "adapter", "binding"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root, package, project = self.fixture(Path(temporary).resolve())
                if kind == "adapter":
                    (package / "adapters/opencode-room-input/transport.mjs").write_bytes(b"changed")
                else:
                    path = root / ("setup.json" if kind == "pending" else "room-input.json")
                    value = json.loads(path.read_text())
                    value["phase" if kind == "pending" else "sessionID"] = "uncertain"
                    path.write_text(json.dumps(value))
                with self.assertRaises(room.SetupError):
                    room.configure(root, package, "foreground", system="darwin")
                self.assertFalse((project / "opencode.json").exists())

    def test_preview_install_does_not_replace_stable_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.dict(os.environ, {"HOME": temporary}), patch.object(install, "version", return_value="0.13.0-preview.1"), patch.object(install, "platform_name", return_value="macos-aarch64"), patch.object(install, "install_preview", return_value=Path("fixture")) as preview, patch.object(install, "install_bundle", side_effect=AssertionError("stable must not be replaced")):
                self.assertEqual(install.main(), 0)
                self.assertEqual(preview.call_args.args[1], Path(temporary) / ".local/share/codr/packages/v0.13.0-preview.1")

    @patch.object(install, "verify_signature")
    @patch.object(install, "probe_binary")
    def test_preview_archive_round_trip_checks_exact_bytes_and_rejects_corruption(self, probe, signature):
        probe.return_value = self.release_info()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            binary, archive, checksum = base / "fixture-binary", base / "asset.tar.gz", base / "checksum"
            self.executable_fixture(binary)
            package_evidence.package(SCRIPTS.parent, binary, archive)
            checksum.write_text(install.sha256(archive) + "  asset.tar.gz\n")
            def download(url, destination):
                shutil.copyfile(checksum if url.endswith(".sha256") else archive, destination)
            destination = base / "installed"
            installed = install.install_preview("https://example.invalid/fixture", destination, downloader=download)
            self.assertEqual(installed.read_bytes(), binary.read_bytes())
            for name in package_evidence.source_files():
                self.assertEqual((destination / name).read_bytes(), (SCRIPTS.parent / name).read_bytes())
            with self.assertRaises(install.InstallError):
                install.install_preview("https://example.invalid/fixture", destination, downloader=download)
            archive.write_bytes(archive.read_bytes() + b"corrupt")
            with self.assertRaises(install.InstallError):
                install.install_preview("https://example.invalid/fixture", base / "corrupt", downloader=download)
            self.assertFalse((base / "corrupt").exists())

    def test_package_file_list_includes_exact_adapter_and_licenses(self):
        names = set(package_evidence.source_files())
        for name in names:
            self.assertTrue((SCRIPTS.parent / name).is_file(), name)
        for name in ("index.mjs", "admission.mjs", "transport.mjs", "package.json"):
            self.assertIn("adapters/opencode-room-input/" + name, names)
        self.assertIn("vendor/mdk/LICENSE", names)
        for name in ("LICENSE-MPL-2.0", "NOTICE.codr", "README.codr.md"):
            self.assertIn("vendor/mdk/" + name, names)
        workflow = (SCRIPTS.parent / ".github/workflows/build.yml").read_text()
        self.assertEqual(workflow.count("run: bun test adapters/opencode-room-input"), 2)
        self.assertIn("flags+=(--prerelease --latest=false)", workflow)
        self.assertIn("needs: [checks, checks-macos]", workflow)
        self.assertIn("macos-15-intel", workflow)
        self.assertIn("cargo test --locked -p agent-control -p agent-connector", workflow)

    @patch.object(install, "verify_signature")
    @patch.object(install, "probe_binary")
    def test_preview_rejects_altered_root_license_even_with_matching_archive_checksum(self, probe, signature):
        probe.return_value = self.release_info()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            binary, archive, altered = base / "binary", base / "original.tar.gz", base / "altered.tar.gz"
            self.executable_fixture(binary)
            package_evidence.package(SCRIPTS.parent, binary, archive)
            with tarfile.open(archive, "r:gz") as source, tarfile.open(altered, "w:gz") as target:
                for entry in source.getmembers():
                    content = source.extractfile(entry).read()
                    if entry.name == "LICENSE-MPL-2.0":
                        content = b"altered license fixture"
                        entry.size = len(content)
                    target.addfile(entry, io.BytesIO(content))
            def download(url, destination):
                if url.endswith(".sha256"):
                    destination.write_text(install.sha256(altered) + "  altered.tar.gz\n")
                else:
                    shutil.copyfile(altered, destination)
            destination = base / "installed"
            with self.assertRaisesRegex(install.InstallError, "source file checksum mismatch"):
                install.install_preview("https://example.invalid/fixture", destination, downloader=download)
            self.assertFalse(destination.exists())

    @patch.object(install, "verify_signature")
    @patch.object(install, "probe_binary")
    def test_packager_refuses_wrong_version_target_source_and_dirty_build(self, probe, signature):
        for field, value in (("version", "0.12.2"), ("target", "wrong-target"),
                             ("source_commit", "0" * 40), ("source_dirty", True)):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary)
                self.executable_fixture(base / "binary")
                info = self.release_info()
                info[field] = value
                probe.return_value = info
                with self.assertRaisesRegex(install.InstallError, "identity differs"):
                    package_evidence.package(SCRIPTS.parent, base / "binary", base / "asset.tar.gz")
                self.assertFalse((base / "asset.tar.gz").exists())

    @patch.object(install, "verify_signature")
    @patch.object(install, "probe_binary")
    def test_installer_refuses_wrong_binary_even_with_rewritten_archive_checksum(self, probe, signature):
        for kind in ("digest", "platform", "non-binary", "runtime-version"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary)
                binary, archive, altered = base / "binary", base / "original.tar.gz", base / "altered.tar.gz"
                self.executable_fixture(binary)
                probe.return_value = self.release_info()
                package_evidence.package(SCRIPTS.parent, binary, archive)
                with tarfile.open(archive, "r:gz") as source:
                    entries = source.getmembers()
                    contents = {entry.name: source.extractfile(entry).read() for entry in entries}
                content = bytearray(contents["herdr-white-noise-bridge"])
                if kind == "digest":
                    content.extend(b"changed")
                elif kind == "platform":
                    # Flip the actual CPU while keeping a plausible object header.
                    if content[:4] == b"\x7fELF":
                        struct.pack_into("<H", content, 18, 183 if install.platform_name().endswith("x86_64") else 62)
                    else:
                        struct.pack_into("<I", content, 4, 0x0100000c if install.platform_name().endswith("x86_64") else 0x01000007)
                elif kind == "non-binary":
                    content = bytearray(b"not an executable")
                else:
                    info = self.release_info()
                    info["version"] = "0.12.2"
                    probe.return_value = info
                contents["herdr-white-noise-bridge"] = bytes(content)
                if kind in ("platform", "non-binary"):
                    evidence = json.loads(contents["CODR-SOURCE.json"])
                    evidence["executable"]["sha256"] = hashlib.sha256(content).hexdigest()
                    contents["CODR-SOURCE.json"] = json.dumps(evidence).encode()
                with tarfile.open(altered, "w:gz") as target:
                    for entry in entries:
                        entry.size = len(contents[entry.name])
                        target.addfile(entry, io.BytesIO(contents[entry.name]))
                def download(url, destination):
                    if url.endswith(".sha256"):
                        destination.write_text(install.sha256(altered) + "  altered.tar.gz\n")
                    else:
                        shutil.copyfile(altered, destination)
                with self.assertRaises(install.InstallError):
                    install.install_preview("https://example.invalid/fixture", base / "installed", downloader=download)
                self.assertFalse((base / "installed").exists())


if __name__ == "__main__":
    unittest.main()
