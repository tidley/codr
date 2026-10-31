import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
import install as installer
import services

spec = importlib.util.spec_from_file_location("codr_setup", SCRIPTS / "setup.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class InstallerTests(unittest.TestCase):
    def test_combined_release_installs_one_executable_without_a_connector_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.dict(os.environ, {"HOME":temporary}), patch.object(installer,"version",return_value="0.10.0"), patch.object(installer,"platform_name",return_value="linux-x86_64"), patch.object(installer,"install_bundle") as install:
                self.assertEqual(installer.main(),0)
            self.assertEqual(install.call_count,1)
            self.assertIn("codr-v0.10.0-linux-x86_64.tar.gz",install.call_args.args[0])
            self.assertEqual(install.call_args.args[1],"herdr-white-noise-bridge")

    def test_release_installs_its_dedicated_connector_before_the_bridge(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.dict(os.environ, {"HOME": temporary}), patch.object(installer, "version", return_value="0.6.0"), patch.object(installer, "platform_name", return_value="linux-x86_64"), patch.object(installer, "install_bundle") as install:
                self.assertEqual(installer.main(), 0)
            self.assertEqual(len(install.call_args_list), 2)
            connector, bridge = [call.args for call in install.call_args_list]
            self.assertIn("codr-wn-agent-v0.6.0-linux-x86_64.tar.gz", connector[0])
            self.assertEqual(connector[1], "wn-agent")
            self.assertEqual(connector[2], Path(temporary) / ".local/share/codr/bin/wn-agent-codr-v0.6.0")
            self.assertIn("codr-v0.6.0-linux-x86_64.tar.gz", bridge[0])
            self.assertEqual(bridge[1], "herdr-white-noise-bridge")

    def test_supported_architectures_and_rejection(self):
        self.assertEqual(installer.platform_name("Linux", "x86_64"), "linux-x86_64")
        self.assertEqual(installer.platform_name("Linux", "arm64"), "linux-aarch64")
        self.assertEqual(installer.platform_name("Darwin", "arm64"), "macos-aarch64")
        self.assertEqual(installer.platform_name("Darwin", "x86_64"), "macos-x86_64")
        self.assertEqual(installer.mdk_platform_name("Darwin", "arm64"), "darwin-aarch64")
        with self.assertRaises(installer.InstallError):
            installer.platform_name("Windows", "x86_64")

    def test_downloaded_archive_is_checked_before_installation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "source.tar.gz"
            binary = b"#!/bin/sh\nprintf 'test binary\\n'\n"
            with tarfile.open(archive, "w:gz") as bundle:
                entry = tarfile.TarInfo("herdr-white-noise-bridge")
                entry.size = len(binary)
                bundle.addfile(entry, io.BytesIO(binary))
            payload = archive.read_bytes()
            checksum = hashlib.sha256(payload).hexdigest().encode() + b"  source.tar.gz\n"

            def download(url, destination):
                destination.write_bytes(checksum if url.endswith(".sha256") else payload)

            target = root / "bin/bridge"
            with patch.object(installer, "download", side_effect=download):
                installer.install_bundle("https://github.com/example/release.tar.gz", "herdr-white-noise-bridge", target)
            self.assertEqual(target.read_bytes(), binary)
            self.assertEqual(target.stat().st_mode & 0o777, 0o755)
            checksum = b"0" * 64 + b"\n"
            with patch.object(installer, "download", side_effect=download):
                with self.assertRaises(installer.InstallError):
                    installer.install_bundle("https://github.com/example/release.tar.gz", "herdr-white-noise-bridge", target)
            self.assertEqual(target.read_bytes(), binary)

    def test_symlink_archive_member_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                entry = tarfile.TarInfo("bridge")
                entry.type = tarfile.SYMTYPE
                entry.linkname = "/etc/passwd"
                bundle.addfile(entry)
            payload = archive.read_bytes()

            def download(url, destination):
                destination.write_bytes(hashlib.sha256(payload).hexdigest().encode() if url.endswith(".sha256") else payload)

            with patch.object(installer, "download", side_effect=download):
                with self.assertRaises(installer.InstallError):
                    installer.install_bundle("https://github.com/example/release.tar.gz", "bridge", root / "installed")
            self.assertFalse((root / "installed").exists())


class ManifestTests(unittest.TestCase):
    def test_release_manifest_declares_linux_and_macos_and_matches_the_package_version(self):
        manifest = setup.tomllib.loads((SCRIPTS.parent / "herdr-plugin.toml").read_text())
        self.assertEqual(manifest["platforms"], ["linux", "macos"])
        self.assertEqual(manifest["version"], setup.tomllib.loads((SCRIPTS.parent / "Cargo.toml").read_text())["package"]["version"])


class SetupTests(unittest.TestCase):
    def test_new_bridge_identity_publishes_the_pat_display_name(self):
        opencode = self.home / ".local/bin/opencode"
        opencode.write_text("fixture")
        opencode.chmod(0o700)
        def find(name, candidates=()):
            return str(opencode) if name == "opencode" else "/usr/bin/" + name
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            report = setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(self.profile_calls, [{"marmot_agent_control":"marmot.agent-control.v2", "type":"account_publish_profile", "account_id_hex":"ab" * 32, "name":"Pat", "display_name":"Pat"}])
        self.assertEqual(report["default_profile_status"], "published")
        self.assertEqual(setup.tomllib.loads(self.config.read_text())["account_id_hex"], "ab" * 32)
        stored = json.loads((self.home / ".marmot-agents/herdr/codr-bootstrap.json").read_text())
        self.assertEqual(stored["label"], "herdr-bridge")
        self.assertEqual(stored["default_profile_status"], "published")
        self.profile_calls.clear()
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            setup.configure(None, self.socket, self.runner)
            setup.configure(None, self.socket, self.runner, check=True)
        self.assertEqual(self.profile_calls, [])

    def test_reused_bridge_account_keeps_its_existing_profile(self):
        account={"label":"herdr-bridge","account_id_hex":"ab" * 32,"local_signing":True}
        with self.mocks(), patch.object(setup, "local_accounts", return_value=[account]):
            report=setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(self.profile_calls, [])
        self.assertEqual(report["default_profile_status"], "preserved")

    def test_an_unconfirmed_profile_is_reported_and_not_republished_on_setup_retry(self):
        def unavailable(path, payload):
            self.profile_calls.append(payload)
            raise TimeoutError("lost acknowledgement")
        with self.mocks(), patch.object(setup, "control_request", side_effect=unavailable):
            report=setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(report["default_profile_status"], "unconfirmed")
        self.assertEqual(len(self.profile_calls), 1)
        self.config.unlink()  # Synthetic interrupted setup; no live configuration is used.
        account={"label":"herdr-bridge","account_id_hex":"ab" * 32,"local_signing":True}
        with self.mocks(), patch.object(setup, "local_accounts", return_value=[account]):
            report=setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(len(self.profile_calls), 1)
        self.assertEqual(report["default_profile_status"], "unconfirmed")

    def test_profile_acknowledgements_must_match_protocol_identity_and_name(self):
        good={"marmot_agent_control":"marmot.agent-control.v2","type":"profile_published","account_id_hex":"ab" * 32,"name":"Pat","display_name":"Pat"}
        for field,wrong in [("marmot_agent_control","wrong"),("type","ack"),("account_id_hex","99" * 32),("name","Other"),("display_name","Other")]:
            with self.subTest(field=field), patch.object(setup,"control_request",return_value={**good,field:wrong}) as request:
                path=self.home / f"profile-{field}.json"
                result=setup.publish_pat_profile(self.socket,{"account_id_hex":"ab" * 32,"created":True},path)
                self.assertEqual(result["default_profile_status"],"unconfirmed")
                result=setup.publish_pat_profile(self.socket,{"account_id_hex":"ab" * 32,"created":True},path)
                self.assertEqual(result["default_profile_status"],"unconfirmed")
                self.assertEqual(request.call_count,1)

    def test_profile_publication_requires_a_saved_intent(self):
        with patch.object(setup,"private_write",side_effect=OSError("cannot save intent")), patch.object(setup,"control_request") as request:
            with self.assertRaises(OSError):
                setup.publish_pat_profile(self.socket,{"account_id_hex":"ab" * 32,"created":True},self.home / "profile.json")
        request.assert_not_called()

    def test_pending_profile_intent_is_not_republished_after_a_crash(self):
        path = self.home / "profile-pending.json"
        path.write_text(json.dumps({"account_id_hex":"ab" * 32,"default_profile_status":"pending"}))
        with patch.object(setup, "control_request") as request:
            result = setup.publish_pat_profile(self.socket, {"account_id_hex":"ab" * 32,"created":True}, path)
        self.assertEqual(result["default_profile_status"], "unconfirmed")
        request.assert_not_called()

    def test_profile_record_cannot_be_reused_for_another_identity(self):
        path = self.home / "profile-other.json"
        path.write_text(json.dumps({"account_id_hex":"99" * 32,"default_profile_status":"published"}))
        with patch.object(setup, "control_request") as request:
            with self.assertRaises(setup.SetupError):
                setup.publish_pat_profile(self.socket, {"account_id_hex":"ab" * 32,"created":True}, path)
        request.assert_not_called()

    def test_fresh_portr_access_is_off_unless_explicitly_selected(self):
        with self.mocks():
            report = setup.configure("npub1fixture", self.socket, self.runner)
        self.assertIs(setup.tomllib.loads(self.config.read_text()).get("portr_enabled", False), False)
        self.assertIs(report["portr_enabled"], False)

    def test_portr_opt_in_does_not_install_or_configure_ssh(self):
        with self.mocks():
            report = setup.configure("npub1fixture", self.socket, self.runner, enable_portr=True)
        self.assertIs(setup.tomllib.loads(self.config.read_text())["portr_enabled"], True)
        self.assertIs(report["portr_enabled"], True)
        self.assertFalse(report["portr_restart_required"])
        self.assertFalse(any(Path(call[0]).name in {"ssh", "sshd", "ssh-keygen"} for call in self.calls))

    def test_phone_only_config_remains_readable_by_releases_without_desktop_fields(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_portr=False)
        self.assertNotIn("portr_enabled", setup.tomllib.loads(self.config.read_text()))

    def test_existing_portr_preference_is_preserved_without_an_option(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_portr=True)
            before = self.config.read_bytes()
            report = setup.configure(None, self.socket, self.runner)
        self.assertEqual(self.config.read_bytes(), before)
        self.assertIs(report["portr_enabled"], True)
        self.assertFalse(report["portr_restart_required"])
        self.assertEqual(self.bootstrap_count, 1)

    def test_existing_portr_option_changes_only_the_setting_without_restarting(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_portr=True)
            original = self.config.read_text() + "# Keep this comment\n"
            self.config.write_text(original)
            for enabled in [False, True]:
                self.calls.clear()
                previous = setup.tomllib.loads(self.config.read_text())["portr_enabled"]
                report = setup.configure(None, self.socket, self.runner, enable_portr=enabled)
                original = original.replace(f"portr_enabled = {str(previous).lower()}", f"portr_enabled = {str(enabled).lower()}")
                self.assertEqual(self.config.read_text(), original)
                self.assertIs(report["portr_enabled"], enabled)
                self.assertTrue(report["portr_restart_required"])
                self.assertFalse(any("restart" in call or "stop" in call for call in self.calls))
        self.assertEqual(self.bootstrap_count, 1)

    def test_check_cannot_apply_a_portr_choice(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
            before = self.config.read_bytes()
            self.calls.clear()
            with self.assertRaises(setup.SetupError):
                setup.configure(None, self.socket, self.runner, check=True, enable_portr=True)
        self.assertEqual(self.config.read_bytes(), before)
        self.assertEqual(self.calls, [])

    def test_portr_update_preserves_an_inline_comment_and_validates_before_writing(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_portr=True)
            original = self.config.read_text().replace("portr_enabled = true", "portr_enabled = false  # optional desktop")
            self.config.write_text(original)
            def fail_candidate(argv):
                if any("codr-portr-check-" in str(arg) for arg in argv):
                    raise setup.SetupError("Invalid candidate")
                return self.runner(argv)
            with self.assertRaises(setup.SetupError):
                setup.configure(None, self.socket, fail_candidate, enable_portr=True)
            self.assertEqual(self.config.read_text(), original)
            setup.configure(None, self.socket, self.runner, enable_portr=True)
        self.assertEqual(self.config.read_text(), original.replace("portr_enabled = false", "portr_enabled = true"))

    def test_old_configs_remain_disabled_and_can_explicitly_add_portr(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
            original = self.config.read_text().replace("portr_enabled = false\n", "") + "# retained\n"
            self.config.write_text(original)
            report = setup.configure(None, self.socket, self.runner)
            self.assertFalse(report["portr_enabled"])
            self.assertEqual(self.config.read_text(), original)
            setup.configure(None, self.socket, self.runner, enable_portr=True)
        self.assertEqual(self.config.read_text(), original + "portr_enabled = true\n")

    def test_portr_setting_inside_a_multiline_command_is_not_changed(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_portr=True)
            original = self.config.read_text()
            setting = next(line for line in original.splitlines() if line.startswith("transcribe_command ="))
            original = original.replace(setting, 'transcribe_command = ["/usr/bin/python3", "-c", \'\'\'\nportr_enabled = true\nprint("keep this command")\n\'\'\']')
            self.config.write_text(original)
            before = setup.tomllib.loads(original)
            report = setup.configure(None, self.socket, self.runner, enable_portr=False)
        after = setup.tomllib.loads(self.config.read_text())
        self.assertFalse(after["portr_enabled"])
        self.assertEqual(after["transcribe_command"], before["transcribe_command"])
        self.assertEqual(after, {**before, "portr_enabled": False})
        self.assertFalse(report["portr_enabled"])

    def test_setup_cli_portr_selection_is_explicit_and_defaults_to_unspecified(self):
        for arguments, expected in [([], None), (["--portr"], True), (["--no-portr"], False)]:
            with self.subTest(arguments=arguments), patch.object(setup, "version", return_value="0.9.0"), patch.object(setup.sys, "argv", ["setup.py", "--npub", "npub1fixture", *arguments]), patch.object(setup.sys.stdin, "isatty", return_value=False), patch.object(setup, "configure", return_value={}) as configure, patch.object(setup.sys, "stdout", io.StringIO()):
                self.assertEqual(setup.main(), 0)
                self.assertIs(configure.call_args.kwargs["enable_portr"], expected)

    def test_interactive_fresh_setup_offers_portr_with_no_as_the_default(self):
        for answer, expected in [("", False), ("n", False), ("yes", True)]:
            with self.subTest(answer=answer), patch.object(setup, "version", return_value="0.9.0"), patch.object(setup.sys, "argv", ["setup.py", "--npub", "npub1fixture"]), patch.object(setup.sys.stdin, "isatty", return_value=True), patch("builtins.input", return_value=answer) as prompt, patch.object(setup, "configure", return_value={}) as configure, patch.object(setup.sys, "stdout", io.StringIO()):
                self.assertEqual(setup.main(), 0)
                prompt.assert_called_once()
                self.assertIn("[y/N]", prompt.call_args.args[0])
                self.assertIs(configure.call_args.kwargs["enable_portr"], expected)

    def test_existing_interactive_setup_does_not_prompt_for_portr_again(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text("# existing setup\n")
        with patch.object(setup, "version", return_value="0.9.0"), patch.object(setup.sys, "argv", ["setup.py"]), patch.object(setup.sys.stdin, "isatty", return_value=True), patch("builtins.input") as prompt, patch.object(setup, "configure", return_value={}) as configure, patch.object(setup.sys, "stdout", io.StringIO()):
            self.assertEqual(setup.main(), 0)
            prompt.assert_not_called()
            self.assertIsNone(configure.call_args.kwargs["enable_portr"])

    def test_combined_version_refuses_legacy_two_service_setup(self):
        with patch.object(setup, "version", return_value="0.10.0"), patch.object(setup.sys, "argv", ["setup.py", "--npub", "npub1fixture"]), patch.object(setup.sys.stdin, "isatty", return_value=False), patch.object(setup, "configure") as configure, patch.object(setup.sys, "stderr", io.StringIO()) as errors:
            self.assertEqual(setup.main(), 1)
            configure.assert_not_called()
            self.assertIn("--combined-home", errors.getvalue())

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary.name)
        self.environment = patch.dict(os.environ, {"HOME": str(self.home), "XDG_CONFIG_HOME": str(self.home / "config space %"), "XDG_STATE_HOME": str(self.home / "state"), "CODR_OWNER_NPUB": "", "CODR_SERVICE_MANAGER": "systemd"}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.addCleanup(self.temporary.cleanup)
        self.bridge = self.home / ".local/bin/herdr-white-noise-bridge"
        self.bridge.parent.mkdir(parents=True)
        self.bridge.write_text("fixture")
        self.socket = self.home / "named-herdr.sock"
        self.calls = []
        self.bootstrap_count = 0
        self.profile_calls = []
        self.config = self.home / "config space %/herdr-white-noise-bridge/config.toml"

    def runner(self, argv):
        self.calls.append([str(a) for a in argv])
        if "bootstrap" in argv:
            self.bootstrap_count += 1
            return json.dumps({"account_id_hex": "ab" * 32, "npub": "npub1bridge", "label": "herdr-bridge", "key_package_published": True, "created":"--no-create" not in argv})
        if "--version" in argv:
            return f"wn-agent {setup.MDK_VERSION}"
        if "check-readers" in argv:
            return json.dumps({"checked_sessions": 0})
        return ""

    def mocks(self):
        return patch.multiple(setup,
            find_executable=lambda name, candidates=(): "/usr/bin/" + name,
            ensure_mdk=lambda runner: Path("/usr/bin/wn-agent"),
            read_snapshot=lambda path: {"agents": []},
            local_accounts=lambda path: [],
            control_request=self.profile_control,
            configure_phonon=lambda *args: ["/usr/bin/python3", "/example/transcribe.py", "--socket", "/example/phonon.sock"])

    def profile_control(self, path, payload):
        self.profile_calls.append(payload)
        self.assertEqual(payload["type"], "account_publish_profile")
        return {"marmot_agent_control":"marmot.agent-control.v2","type":"profile_published","account_id_hex":payload["account_id_hex"],"name":"Pat","display_name":"Pat"}

    def test_connector_selection_uses_the_release_owned_path_not_a_shared_executable(self):
        dedicated = installer.connector_path(installer.version())
        def install(release):
            self.assertEqual(release, installer.version())
            dedicated.parent.mkdir(parents=True, exist_ok=True)
            dedicated.write_text("fixture")
        with patch.object(setup, "install_connector", side_effect=install) as download:
            self.assertEqual(setup.ensure_mdk(lambda argv: f"wn-agent {setup.MDK_VERSION}"), dedicated)
            self.assertEqual(setup.ensure_mdk(lambda argv: f"wn-agent {setup.MDK_VERSION}"), dedicated)
            self.assertEqual(download.call_count, 1)

    def test_managed_transport_upgrade_changes_only_the_executable(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        existing = setup.tomllib.loads(self.config.read_text())
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_text()
        binary = installer.connector_path(installer.version())
        before_config = self.config.read_bytes()
        with patch.object(setup, "ensure_mdk", return_value=binary):
            self.assertTrue(setup.upgrade_transport(existing, self.config.parents[1], self.runner))
        self.assertEqual(unit.read_text(), original.replace(setup.unit_argument("/usr/bin/wn-agent"), setup.unit_argument(binary), 1))
        self.assertEqual(self.config.read_bytes(), before_config)
        self.assertIn(["systemctl", "--user", "stop", "herdr-white-noise-bridge.service"], self.calls)
        self.assertIn(["systemctl", "--user", "start", "wn-agent-herdr.service"], self.calls)

    def test_failed_transport_upgrade_restores_the_unit_and_attempts_both_services(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        existing = setup.tomllib.loads(self.config.read_text())
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        failed = False
        def runner(argv):
            nonlocal failed
            self.calls.append(argv)
            if "--version" in argv:
                return f"wn-agent {setup.MDK_VERSION}"
            if argv == ["systemctl", "--user", "start", "wn-agent-herdr.service"] and not failed:
                failed = True
                raise setup.SetupError("fixture startup failure")
            return ""
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaises(setup.SetupError):
                setup.upgrade_transport(existing, self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertEqual(self.calls[-2:], [["systemctl", "--user", "start", "wn-agent-herdr.service"], ["systemctl", "--user", "start", "herdr-white-noise-bridge.service"]])

    def test_custom_transport_overrides_are_not_replaced(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        overrides = unit.with_name(unit.name + ".d")
        overrides.mkdir()
        override = overrides / "custom.conf"
        override.write_text("[Service]\nEnvironment=KEEP_ME=1\n")
        self.calls.clear()
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaises(setup.SetupError):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], self.runner)
        self.assertEqual(override.read_text(), "[Service]\nEnvironment=KEEP_ME=1\n")
        self.assertFalse(self.calls)

    def test_inactive_services_after_successful_start_trigger_systemd_rollback(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        def runner(argv):
            if "is-active" in argv:
                raise setup.SetupError("fixture activation failure")
            return self.runner(argv)
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaises(setup.SetupError):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertEqual(self.calls[-2:], [["systemctl", "--user", "start", "wn-agent-herdr.service"], ["systemctl", "--user", "start", "herdr-white-noise-bridge.service"]])

    def test_failed_bridge_start_rolls_back_the_running_connector_not_just_its_unit(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        running = {"transport": original}
        failed = False
        def runner(argv):
            nonlocal failed
            if "--version" in argv:
                return f"wn-agent {setup.MDK_VERSION}"
            if argv == ["systemctl", "--user", "stop", "wn-agent-herdr.service"]:
                running["transport"] = None
            elif argv == ["systemctl", "--user", "start", "wn-agent-herdr.service"] and running["transport"] is None:
                running["transport"] = unit.read_bytes()
            elif argv == ["systemctl", "--user", "start", "herdr-white-noise-bridge.service"] and not failed:
                failed = True
                self.assertNotEqual(running["transport"], original)
                raise setup.SetupError("bridge startup failed after transport activation")
            return ""
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaises(setup.SetupError):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertEqual(running["transport"], original)

    def test_effective_runtime_and_system_dropins_are_refused_before_stopping_services(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        for external in ("/run/user/123/systemd/user/wn-agent-herdr.service.d/runtime.conf", "/etc/systemd/user/wn-agent-herdr.service.d/system.conf"):
            calls = []
            def runner(argv):
                calls.append(argv)
                return external if "DropInPaths" in argv else ""
            with self.subTest(external=external), patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
                with self.assertRaises(setup.SetupError):
                    setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
                self.assertEqual(unit.read_bytes(), original)
                self.assertFalse(any("stop" in argv for argv in calls))

    def test_unknown_existing_connector_cohorts_are_not_downgraded(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        calls = []
        def runner(argv):
            calls.append(argv)
            return "wn-agent 0.15.0" if "--version" in argv else ""
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaisesRegex(setup.SetupError, "supported MDK cohort"):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertFalse(any("stop" in argv for argv in calls))

    def test_013_storage_requires_manual_migration_before_services_are_stopped(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        calls = []
        def runner(argv):
            calls.append(argv)
            return "wn-agent 0.13.0" if "--version" in argv else ""
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaisesRegex(setup.SetupError, "supported MDK cohort"):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertFalse(any("stop" in argv for argv in calls))

    def test_interrupted_upgrade_restores_services_before_propagating_interrupt(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        original = unit.read_bytes()
        calls = []
        interrupted = False
        def runner(argv):
            nonlocal interrupted
            calls.append(argv)
            if "--version" in argv:
                return f"wn-agent {setup.MDK_VERSION}"
            if argv == ["systemctl", "--user", "stop", "wn-agent-herdr.service"] and not interrupted:
                interrupted = True
                raise KeyboardInterrupt()
            return ""
        with patch.object(setup, "ensure_mdk", return_value=installer.connector_path(installer.version())):
            with self.assertRaises(KeyboardInterrupt):
                setup.upgrade_transport(setup.tomllib.loads(self.config.read_text()), self.config.parents[1], runner)
        self.assertEqual(unit.read_bytes(), original)
        self.assertEqual(calls[-2:], [["systemctl", "--user", "start", "wn-agent-herdr.service"], ["systemctl", "--user", "start", "herdr-white-noise-bridge.service"]])

    def test_fresh_setup_generates_private_portable_config_and_preserves_on_repeat(self):
        with self.mocks():
            result = setup.configure("npub1fixture", self.socket, self.runner)
            self.assertTrue(self.config.exists())
            self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
            config = setup.tomllib.loads(self.config.read_text())
            self.assertEqual(config["phone_npub"], "npub1fixture")
            self.assertEqual(config["herdr_socket"], str(self.socket.resolve()))
            self.assertEqual(config["account_id_hex"], "ab" * 32)
            unit = self.config.parents[1] / "systemd/user/herdr-white-noise-bridge.service"
            self.assertIn("%%", unit.read_text())
            self.assertIn("--config", unit.read_text())
            original = self.config.read_bytes()
            setup.configure("npub1fixture", self.socket, self.runner)
            self.assertEqual(self.config.read_bytes(), original)
            self.assertEqual(self.bootstrap_count, 1)
            self.assertNotIn("restart", [part for call in self.calls for part in call])

    def test_wrong_owner_cannot_change_an_existing_installation(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
            original = self.config.read_bytes()
            with self.assertRaises(setup.SetupError):
                setup.configure("npub1different", self.socket, self.runner)
            self.assertEqual(self.config.read_bytes(), original)

    def test_enabling_phonon_on_an_existing_text_only_install_keeps_one_valid_command(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner, enable_audio=False)
            setup.configure("npub1fixture", self.socket, self.runner, enable_audio=True)
        config = setup.tomllib.loads(self.config.read_text())
        self.assertEqual(config["phone_npub"], "npub1fixture")
        self.assertEqual(config["transcribe_command"][0], "/usr/bin/python3")
        self.assertEqual(self.bootstrap_count, 1)

    def test_invalid_owner_stops_before_services_or_identity_creation(self):
        def runner(argv):
            self.calls.append(argv)
            raise setup.SetupError("Invalid owner")
        with self.mocks():
            with self.assertRaises(setup.SetupError):
                setup.configure("npub1invalid", self.socket, runner)
        self.assertFalse(self.config.exists())
        self.assertFalse((self.home / ".marmot-agents/herdr").exists())
        self.assertTrue(all("systemctl" not in str(call) for call in self.calls))

    def test_reader_failure_stops_fresh_setup_before_transport_or_identity_creation(self):
        def runner(argv):
            if "check-readers" in argv:
                raise setup.SetupError("Session history check failed")
            return self.runner(argv)
        with self.mocks():
            with self.assertRaisesRegex(setup.SetupError, "history check"):
                setup.configure("npub1fixture", self.socket, runner)
        self.assertEqual(self.bootstrap_count, 0)
        self.assertFalse(self.config.exists())
        self.assertFalse(any(call[:3] in (["systemctl", "--user", "start"], ["systemctl", "--user", "stop"]) for call in self.calls))

    def test_existing_reader_failure_stops_before_connector_upgrade(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        self.calls.clear()
        def runner(argv):
            if "check-readers" in argv:
                raise setup.SetupError("Session history check failed")
            return self.runner(argv)
        with self.mocks(), patch.object(setup, "upgrade_transport") as upgrade:
            with self.assertRaisesRegex(setup.SetupError, "history check"):
                setup.configure("npub1fixture", self.socket, runner)
            upgrade.assert_not_called()

    def test_setup_check_reports_deferred_readers_without_changing_services_or_config(self):
        opencode = self.home / ".local/bin/opencode"
        opencode.write_text("fixture")
        opencode.chmod(0o700)
        def find(name, candidates=()):
            return str(opencode) if name == "opencode" else "/usr/bin/" + name
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            setup.configure("npub1fixture", self.socket, self.runner)
        before = self.config.read_bytes()
        self.calls.clear()
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find), patch.object(setup, "upgrade_transport") as upgrade:
            result = setup.configure("npub1fixture", self.socket, self.runner, check=True)
            upgrade.assert_not_called()
        self.assertEqual(result["checked_session_readers"], 0)
        self.assertEqual(result["reader_validation"], "deferred: no live supported sessions")
        self.assertEqual(self.config.read_bytes(), before)
        self.assertFalse(any(call[0] in ("systemctl", "launchctl") or "bootstrap" in call for call in self.calls))

    def test_setup_reports_the_number_of_validated_live_readers(self):
        def runner(argv):
            if "check-readers" in argv:
                return json.dumps({"checked_sessions": 3})
            return self.runner(argv)
        with self.mocks():
            result = setup.configure("npub1fixture", self.socket, runner)
        self.assertEqual(result["checked_session_readers"], 3)
        self.assertEqual(result["reader_validation"], "passed")

    def test_resume_selects_existing_account_without_creating_another(self):
        with self.mocks(), patch.object(setup, "local_accounts", return_value=[{"label": "herdr-bridge", "account_id_hex": "ab" * 32, "local_signing": True}]):
            setup.configure("npub1fixture", self.socket, self.runner)
        bootstrap = next(call for call in self.calls if "bootstrap" in call)
        self.assertIn("--no-create", bootstrap)
        self.assertIn("--account-id-hex", bootstrap)

    def test_concurrent_setup_cannot_create_a_second_identity(self):
        with setup.setup_lock(self.config.parent), self.mocks():
            with self.assertRaises(setup.SetupError):
                setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(self.bootstrap_count, 0)

    def test_existing_install_cannot_silently_change_the_selected_server(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
            with self.assertRaises(setup.SetupError):
                setup.configure("npub1fixture", self.home / "different.sock", self.runner)

    def test_unit_arguments_escape_literal_systemd_environment_references(self):
        self.assertEqual(setup.unit_argument("/config/${CODR_PROFILE}%"), '"/config/$${CODR_PROFILE}%%"')

    def test_retry_after_worker_start_failure_returns_the_same_bridge_identity(self):
        def fail_worker_start(argv):
            if list(argv) == ["systemctl", "--user", "start", "herdr-white-noise-bridge.service"]:
                raise setup.SetupError("Worker start failed")
            return self.runner(argv)
        with self.mocks():
            with self.assertRaises(setup.SetupError):
                setup.configure("npub1fixture", self.socket, fail_worker_start)
            self.assertTrue(self.config.exists())
            result = setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(result["bridge_npub"], "npub1bridge")
        self.assertEqual(self.bootstrap_count, 1)

    def test_codex_only_install_does_not_require_opencode(self):
        def find(name, candidates=()):
            if name in ("opencode", "claude"):
                raise setup.SetupError("Not installed")
            return "/usr/bin/" + name
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            setup.configure("npub1fixture", self.socket, self.runner)
            setup.configure("npub1fixture", self.socket, self.runner)
            setup.configure("npub1fixture", self.socket, self.runner, check=True)
            self.config.write_text(self.config.read_text().replace('opencode_bin = "opencode"', 'opencode_bin = "/removed/opencode"'))
            setup.configure("npub1fixture", self.socket, self.runner)
            setup.configure("npub1fixture", self.socket, self.runner, check=True)
        config = setup.tomllib.loads(self.config.read_text())
        self.assertEqual(config["opencode_bin"], "/removed/opencode")
        self.assertEqual(config["codex_home"], str((self.home / ".codex").resolve()))

    def test_claude_only_install_does_not_require_opencode_or_codex(self):
        def find(name, candidates=()):
            if name in ("opencode", "codex"):
                raise setup.SetupError("Not installed")
            return "/usr/bin/" + name
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            result = setup.configure("npub1fixture", self.socket, self.runner)
            setup.configure("npub1fixture", self.socket, self.runner)
            setup.configure("npub1fixture", self.socket, self.runner, check=True)
        config = setup.tomllib.loads(self.config.read_text())
        self.assertEqual(config["claude_home"], str((self.home / ".claude").resolve()))
        self.assertEqual(result["supported_agents"], ["claude"])

    def test_macos_setup_writes_launch_agents_and_uses_launchctl(self):
        agents = self.home / "LaunchAgents"
        with self.mocks(), patch.dict(os.environ, {"CODR_SERVICE_MANAGER": "launchd", "CODR_LAUNCH_AGENTS_DIR": str(agents)}), \
                patch.object(services, "_capture", return_value="state = running"):
            result = setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(result["bridge_npub"], "npub1bridge")
        self.assertTrue((agents / "io.github.tidley.codr.wn-agent-herdr.plist").exists())
        self.assertTrue((agents / "io.github.tidley.codr.herdr-white-noise-bridge.plist").exists())
        self.assertFalse((self.config.parents[1] / "systemd").exists())
        self.assertTrue(all("systemctl" not in " ".join(call) for call in self.calls))
        self.assertIn(["launchctl", "kickstart", f"gui/{os.getuid()}/io.github.tidley.codr.herdr-white-noise-bridge"], self.calls)
        plist = plistlib.loads((agents / "io.github.tidley.codr.herdr-white-noise-bridge.plist").read_bytes())
        self.assertIn("--config", plist["ProgramArguments"])
        self.assertEqual(plist["KeepAlive"], {"SuccessfulExit": False})

    def test_fresh_setup_allows_only_the_private_outbound_image_spool(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        config = setup.tomllib.loads(self.config.read_text())
        spool = Path(config["state_path"]).with_suffix(".outbound-media")
        unit = self.config.parents[1] / "systemd/user/wn-agent-herdr.service"
        self.assertIn(setup.unit_argument("--media-allowed-root") + " " + setup.unit_argument(spool), unit.read_text())
        self.assertEqual(spool.stat().st_mode & 0o777, 0o700)

    def test_broken_unrelated_cli_does_not_block_an_observed_native_agent(self):
        def find(name, candidates=()):
            if name in ("codex", "claude"):
                raise setup.SetupError("Not on PATH")
            return "/usr/bin/" + name
        def runner(argv):
            if argv == ["/usr/bin/opencode", "--version"]:
                raise setup.SetupError("Broken unrelated installation")
            return self.runner(argv)
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find), patch.object(setup,"read_snapshot", return_value={"agents":[{"agent":"claude","agent_session":{"agent":"claude"}}]}):
            result=setup.configure("npub1fixture", self.socket, runner)
        self.assertEqual(result["supported_agents"],["claude"])

    def test_launchd_transport_upgrade_preserves_arguments_and_rolls_back_running_jobs(self):
        for failure in (None, setup.SetupError, KeyboardInterrupt, "activation"):
            with self.subTest(failure=failure), patch.dict(os.environ, {"CODR_SERVICE_MANAGER": "launchd", "CODR_LAUNCH_AGENTS_DIR": str(self.home / "LaunchAgents")}):
                manager = services.LaunchdServices(self.runner, setup.private_write, self.config.parents[1])
                home = self.home / ".marmot-agents/herdr"
                argv = ["/usr/bin/wn-agent", "--home", str(home), "--socket", str(home / "dev/wn-agent.sock"), "--relay", "wss://fixture.invalid"]
                unit = manager.path("wn-agent-herdr")
                original = plistlib.dumps(manager.payload(services.Service("wn-agent-herdr", "fixture", argv, {"KEEP_ME": "yes"}))).decode()
                setup.private_write(unit, original)
                existing = {"wn_agent_socket": argv[4], "state_path": str(self.home / "state.json")}
                loaded = {"wn-agent-herdr": True, "herdr-white-noise-bridge": True}
                running = {"transport": original}
                failed = False
                def runner(args):
                    if "--version" in args:
                        return f"wn-agent {setup.MDK_VERSION}"
                    self.assertEqual(args[:2], ["launchctl", "bootout"])
                    name = args[2].split(services.LABEL_PREFIX + ".", 1)[1]
                    loaded[name] = False
                    if name == "wn-agent-herdr":
                        running["transport"] = None
                    return ""
                def start(name):
                    nonlocal failed
                    self.assertFalse(loaded[name])
                    loaded[name] = True
                    if name == "wn-agent-herdr":
                        running["transport"] = unit.read_text()
                    elif failure and failure != "activation" and not failed:
                        failed = True
                        raise failure("fixture interruption")
                binary = installer.connector_path(installer.version())
                with patch.object(setup, "ensure_mdk", return_value=binary), patch.object(setup.services, "for_host", return_value=manager), patch.object(manager, "loaded", side_effect=lambda name: loaded[name]), patch.object(manager, "domain", return_value="gui/123"), patch.object(manager, "start", side_effect=start), patch.object(manager, "is_active", side_effect=setup.SetupError("fixture activation failure") if failure == "activation" else None):
                    if failure:
                        with self.assertRaises(setup.SetupError if failure == "activation" else failure):
                            setup.upgrade_transport(existing, self.config.parents[1], runner)
                        self.assertEqual(unit.read_text(), original)
                        self.assertEqual(running["transport"], original)
                    else:
                        self.assertTrue(setup.upgrade_transport(existing, self.config.parents[1], runner))
                        payload = plistlib.loads(unit.read_bytes())
                        self.assertEqual(payload["ProgramArguments"], [str(binary), *argv[1:]])
                        self.assertEqual(payload["EnvironmentVariables"]["KEEP_ME"], "yes")
                        self.assertEqual(running["transport"], unit.read_text())
                    self.assertTrue(all(loaded.values()))

    def test_find_executable_keeps_the_path_entry_instead_of_the_symlink_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            real = root / "real-opencode"
            real.write_text("fixture")
            real.chmod(0o700)
            link = root / "opencode"
            link.symlink_to(real)
            with patch.dict(os.environ, {"PATH": str(root)}):
                self.assertEqual(setup.find_executable("opencode"), str(link))

    def test_existing_install_repairs_a_moved_opencode_executable(self):
        opencode = self.home / ".local/bin/opencode"
        opencode.write_text("fixture")
        opencode.chmod(0o700)
        def find(name, candidates=()):
            return str(opencode) if name == "opencode" else "/usr/bin/" + name
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find):
            setup.configure("npub1fixture", self.socket, self.runner)
            self.assertEqual(setup.tomllib.loads(self.config.read_text())["opencode_bin"], str(opencode))
            text = self.config.read_text().replace(f'opencode_bin = "{opencode}"', 'opencode_bin = "/missing/opencode"')
            self.config.write_text(text)
            self.calls.clear()
            setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(setup.tomllib.loads(self.config.read_text())["opencode_bin"], str(opencode))
        self.assertIn(["systemctl", "--user", "restart", "herdr-white-noise-bridge.service"], self.calls)

    def test_check_reports_a_missing_opencode_executable_without_writing(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        text = self.config.read_text().replace('opencode_bin = "/usr/bin/opencode"', 'opencode_bin = "/missing/opencode"')
        self.config.write_text(text)
        with self.mocks(), self.assertRaises(setup.SetupError):
            setup.configure("npub1fixture", self.socket, self.runner, check=True)
        self.assertIn('"/missing/opencode"', self.config.read_text())

    def test_check_keeps_a_valid_opencode_path_command_without_repair(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        executable = self.home / ".local/bin/opencode"
        executable.write_text("fixture")
        executable.chmod(0o700)
        self.config.write_text(self.config.read_text().replace('opencode_bin = "/usr/bin/opencode"', 'opencode_bin = "opencode"'))
        before = self.config.read_bytes()
        self.calls.clear()
        with self.mocks(), patch.dict(os.environ, {"PATH": str(executable.parent)}), patch.object(setup, "find_executable") as find:
            setup.configure("npub1fixture", self.socket, self.runner, check=True)
            find.assert_not_called()
        self.assertEqual(self.config.read_bytes(), before)
        self.assertFalse(any(call[0] in ("systemctl", "launchctl") for call in self.calls))

    def test_failed_repair_reader_validation_preserves_config_and_retries_restart(self):
        executable = self.home / ".local/bin/opencode"
        executable.write_text("fixture")
        executable.chmod(0o700)
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        self.config.write_text(self.config.read_text().replace('opencode_bin = "/usr/bin/opencode"', 'opencode_bin = "/missing/opencode"'))
        before = self.config.read_bytes()
        self.calls.clear()
        def find(name, candidates=()):
            return str(executable) if name == "opencode" else "/usr/bin/" + name
        def fail_reader(argv):
            if "check-readers" in argv:
                candidate = Path(argv[argv.index("--config") + 1])
                self.assertEqual(setup.tomllib.loads(candidate.read_text())["opencode_bin"], str(executable))
                raise setup.SetupError("Reader unavailable")
            return self.runner(argv)
        with self.mocks(), patch.object(setup, "find_executable", side_effect=find), patch.object(setup, "upgrade_transport") as upgrade:
            with self.assertRaisesRegex(setup.SetupError, "history check"):
                setup.configure("npub1fixture", self.socket, fail_reader)
            self.assertEqual(self.config.read_bytes(), before)
            self.assertFalse(any(call[0] in ("systemctl", "launchctl") for call in self.calls))
            upgrade.assert_not_called()
            setup.configure("npub1fixture", self.socket, self.runner)
        self.assertEqual(setup.tomllib.loads(self.config.read_text())["opencode_bin"], str(executable))
        self.assertIn(["systemctl", "--user", "restart", "herdr-white-noise-bridge.service"], self.calls)

    def test_failed_repair_restart_restores_config_so_retry_restarts_again(self):
        executable = self.home / ".local/bin/opencode"
        executable.write_text("fixture")
        executable.chmod(0o700)
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        self.config.write_text(self.config.read_text().replace('opencode_bin = "/usr/bin/opencode"', 'opencode_bin = "/missing/opencode"'))
        before = self.config.read_bytes()
        restart = ["systemctl", "--user", "restart", "herdr-white-noise-bridge.service"]
        def runner(argv):
            if argv == restart:
                raise setup.SetupError("Restart failed")
            return self.runner(argv)
        with self.mocks(), patch.object(setup, "find_executable", return_value=str(executable)), patch.object(setup, "upgrade_transport"):
            with self.assertRaisesRegex(setup.SetupError, "Restart failed"):
                setup.configure("npub1fixture", self.socket, runner)
            self.assertEqual(self.config.read_bytes(), before)
            self.calls.clear()
            setup.configure("npub1fixture", self.socket, self.runner)
        self.assertIn(restart, self.calls)

    def test_unavailable_opencode_still_fails_a_required_live_reader(self):
        with self.mocks():
            setup.configure("npub1fixture", self.socket, self.runner)
        self.config.write_text(self.config.read_text().replace('opencode_bin = "/usr/bin/opencode"', 'opencode_bin = "/missing/opencode"'))
        before = self.config.read_bytes()
        self.calls.clear()
        def runner(argv):
            if "check-readers" in argv:
                raise setup.SetupError("OpenCode reader unavailable")
            return self.runner(argv)
        with self.mocks(), patch.object(setup, "find_executable", side_effect=setup.SetupError("Not installed")), patch.object(setup, "upgrade_transport") as upgrade:
            with self.assertRaisesRegex(setup.SetupError, "history check"):
                setup.configure("npub1fixture", self.socket, runner)
            upgrade.assert_not_called()
        self.assertEqual(self.config.read_bytes(), before)
        self.assertFalse(any(call[0] in ("systemctl", "launchctl") for call in self.calls))


if __name__ == "__main__":
    unittest.main()
