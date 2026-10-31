import copy
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import plistlib
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import update as updater


AUTHOR = "2f5759825226f1d57ef1652ba66114b2f938f7f5c50dc505708e5d8b31e4f3c9"
APP = f"32267:{AUTHOR}:codr"


def release(version="0.7.1", channel="main"):
    return {"author": AUTHOR, "application_coordinate": APP,
            "application_identifier": "codr", "version": version,
            "channel": channel, "event_id": "ab" * 32,
            "commit": "cd" * 20, "validation": []}


def asset(name, payload):
    digest = hashlib.sha256(payload).hexdigest()
    return {"author": AUTHOR, "application_coordinate": APP, "version": "0.7.1",
            "filename": name, "sha256": digest, "size": str(len(payload)),
            "url": f"https://blossom.example/{digest}", "validation": [],
            "resolution": "resolved", "platforms": ["linux-x86_64"]}


def fixture_view(payload=b"archive"):
    name = "codr-v0.7.1-linux-x86_64.tar.gz"
    checksum = f"{hashlib.sha256(payload).hexdigest()}  dist/{name}\n".encode()
    return {"release": release(), "unresolved_asset_ids": [],
            "assets": [asset(name, payload), asset(name + ".sha256", checksum)]}


class DiscoveryTests(unittest.TestCase):
    def test_ngit_cli_uses_a_private_metadata_context_and_explicit_pinned_repository(self):
        response = {"format_version": 2, "command_status": "ok", "result": {"releases": []}}
        with patch.object(updater.shutil, "which", return_value="/tools/ngit"), patch.object(updater, "run", side_effect=["", json.dumps(response)]) as run:
            self.assertEqual(updater.ngit(["release", "list"]), {"releases": []})
        argv = run.call_args_list[1].args[0]
        self.assertEqual(argv, ["/tools/ngit", "--repo", updater.REPO, "release", "list", "--json"])
        self.assertNotIn("--signer", argv)

    def test_ngit_discovery_pins_author_event_and_platform_without_github(self):
        view = fixture_view()
        replies = [{"releases": [release("0.8.0", "preview"), release()]}, view]
        with patch.object(updater, "ngit", side_effect=replies) as ngit, patch.object(updater.install, "platform_name", return_value="linux-x86_64"):
            offer = updater.discover("0.7.0")
        self.assertEqual(offer["version"], "0.7.1")
        self.assertEqual(ngit.call_args_list[1].args[0], ["release", "view", "ab" * 32])
        self.assertNotIn("github", json.dumps(offer).lower())

    def test_installed_newer_or_equal_version_never_downgrades(self):
        with patch.object(updater, "ngit", return_value={"releases": [release()]}):
            self.assertIsNone(updater.discover("0.7.1"))
            self.assertIsNone(updater.discover("0.9.0"))

    def test_untrusted_preview_missing_checksum_and_wrong_platform_are_rejected(self):
        for mutate in [
            lambda v: v["release"].update(author="00" * 32),
            lambda v: v["release"].update(channel="preview"),
            lambda v: v["release"].update(validation=["invalid"]),
            lambda v: v.update(unresolved_asset_ids=["missing"]),
            lambda v: v["assets"].pop(),
            lambda v: v["assets"][0].update(platforms=["macos-aarch64"]),
            lambda v: v["assets"][0].update(author="00" * 32),
            lambda v: v["assets"][0].update(version="0.8.0"),
            lambda v: v["assets"][0].update(sha256="bad"),
            lambda v: v["assets"][0].update(url="https://github.com/asset"),
        ]:
            view = fixture_view()
            mutate(view)
            with self.subTest(view=view), patch.object(updater.install, "platform_name", return_value="linux-x86_64"):
                with self.assertRaises(updater.UpdateError):
                    updater.validate_view(view)

    def test_confirmation_cannot_follow_replaced_release_records(self):
        with patch.object(updater.install, "platform_name", return_value="linux-x86_64"):
            offer = updater.validate_view(fixture_view())
            changed = fixture_view(b"replacement")
            with self.assertRaises(updater.UpdateError):
                updater.validate_view(changed, offer)

    def test_download_redirects_never_fall_back_to_github_or_plain_http(self):
        for url in ["https://github.com/a", "https://release-assets.githubusercontent.com/a", "http://blossom.example/a", "https://user:password@blossom.example/a"]:
            with self.subTest(url=url), self.assertRaises(updater.UpdateError):
                updater.blob_url(url)
        with self.assertRaises(updater.UpdateError):
            updater.BlobRedirect().redirect_request(None, None, 302, "redirect", {}, "https://github.com/a")

    def test_blob_download_checks_signed_hash_size_and_checksum_filename(self):
        class Response(io.BytesIO):
            url = "https://blossom.example/blob"

        with tempfile.TemporaryDirectory() as temporary, patch.object(updater.install, "platform_name", return_value="linux-x86_64"):
            offer = updater.validate_view(fixture_view())
            target = Path(temporary) / "download"
            for payload in (b"archive", b"corrupt", b"archive-too-long"):
                with patch.object(updater.urllib.request, "build_opener") as opener:
                    opener.return_value.open.return_value = Response(payload)
                    if payload == b"archive":
                        updater.download_blob(offer, offer["asset"]["url"], target)
                        self.assertEqual(target.read_bytes(), payload)
                    else:
                        with self.assertRaises(updater.UpdateError):
                            updater.download_blob(offer, offer["asset"]["url"], target)
            wrong = b"0" * 64 + b"  another-project.tar.gz\n"
            offer["checksum"]["size"] = str(len(wrong))
            offer["checksum"]["sha256"] = hashlib.sha256(wrong).hexdigest()
            with patch.object(updater.urllib.request, "build_opener") as opener:
                opener.return_value.open.return_value = Response(wrong)
                with self.assertRaises(updater.UpdateError):
                    updater.download_blob(offer, offer["checksum"]["url"], target)

    def test_existing_installer_accepts_separate_blossom_checksum_url_and_pinned_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "fixture.tar.gz"
            binary = b"#!/bin/sh\nexit 0\n"
            with tarfile.open(archive, "w:gz") as bundle:
                entry = tarfile.TarInfo("herdr-white-noise-bridge")
                entry.size = len(binary)
                bundle.addfile(entry, io.BytesIO(binary))
            payload = archive.read_bytes()
            digest = hashlib.sha256(payload).hexdigest()
            checksum = digest.encode() + b"  fixture.tar.gz\n"
            def download(url, destination):
                destination.write_bytes(checksum if url.endswith("/checksum") else payload)
            target = root / "worker"
            updater.install.install_bundle("https://blossom.example/archive", updater.WORKER, target,
                                           checksum_url="https://blossom.example/checksum", expected_sha256=digest,
                                           downloader=download)
            self.assertEqual(target.read_bytes(), binary)
            with self.assertRaises(updater.install.InstallError):
                updater.install.install_bundle("https://blossom.example/archive", updater.WORKER, target,
                                               checksum_url="https://blossom.example/checksum", expected_sha256="00" * 32,
                                               downloader=download)
            self.assertEqual(target.read_bytes(), binary)


class FakeController:
    def __init__(self, healthy=True, rollback_healthy=True):
        self.calls = []
        self.healthy = healthy
        self.rollback_healthy = rollback_healthy

    def validate(self, request):
        self.calls.append("validate")

    def stop(self):
        self.calls.append("stop")

    def start(self):
        self.calls.append("start")

    def wait_ready(self, directory, version, job_id):
        self.calls.append("health:" + version)
        return self.healthy if version == "0.7.1" else self.rollback_healthy


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.job = self.root / "job"
        self.job.mkdir(mode=0o700)
        self.binary = self.root / "bridge"
        self.binary.write_bytes(b"old executable")
        self.binary.chmod(0o755)
        self.state = self.root / "state.json"
        self.state.write_bytes(b'{"mappings":{"chat":{"pending_prompt":true}}}')
        self.config = self.root / "config.toml"
        self.config.write_bytes(b"private existing configuration")
        self.info = {"version": "0.7.0", "state_version": 1, "connector": "pinned-cohort", "protocol": "v2"}
        self.payload = b"new executable"
        self.view = fixture_view()
        with patch.object(updater.install, "platform_name", return_value="linux-x86_64"):
            offer = updater.validate_view(self.view)
        self.request = {"job_id": "job", "release": offer, "installed_path": str(self.binary),
                        "state_path": str(self.state), "config_path": str(self.config),
                        "base_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(), "info": self.info}

    def perform(self, controller, candidate_info=None, download_error=False):
        def stage(url, member, destination, **kwargs):
            if download_error:
                raise updater.UpdateError("Download failed")
            destination.write_bytes(self.payload)
            destination.chmod(0o755)

        candidate_info = candidate_info or {**self.info, "version": "0.7.1"}
        with patch.object(updater, "ngit", return_value=self.view), patch.object(updater.install, "platform_name", return_value="linux-x86_64"), patch.object(updater.install, "install_bundle", side_effect=stage), patch.object(updater, "probe", return_value=candidate_info), patch.object(updater, "check_state"):
            return updater.perform(self.job, self.request, controller)

    def test_success_backs_up_stopped_state_and_replaces_only_executable(self):
        state = self.state.read_bytes()
        config = self.config.read_bytes()
        controller = FakeController()
        result = self.perform(controller)
        self.assertEqual(result["status"], "installed")
        self.assertEqual(self.binary.read_bytes(), self.payload)
        self.assertEqual((self.job / "previous").read_bytes(), b"old executable")
        self.assertEqual((self.job / "state-backup.json").read_bytes(), state)
        self.assertEqual(self.state.read_bytes(), state)
        self.assertEqual(self.config.read_bytes(), config)
        self.assertEqual(controller.calls, ["validate", "validate", "stop", "start", "health:0.7.1"])

    def test_failed_download_never_stops_worker(self):
        controller = FakeController()
        result = self.perform(controller, download_error=True)
        self.assertEqual(result["status"], "failed")
        self.assertNotIn("stop", controller.calls)
        self.assertEqual(self.binary.read_bytes(), b"old executable")

    def test_stale_offer_cannot_downgrade_a_newer_running_bridge(self):
        self.request["info"]["version"] = "0.7.2"
        controller = FakeController()
        result = self.perform(controller)
        self.assertEqual(result["status"], "failed")
        self.assertNotIn("stop", controller.calls)

    def test_changed_connector_or_state_cohort_is_not_installed(self):
        for key, value in [("connector", "new-storage"), ("state_version", 2), ("protocol", "v3"), ("version", "0.7.2")]:
            controller = FakeController()
            result = self.perform(controller, {**self.info, "version": "0.7.1", key: value})
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("stop", controller.calls)
            self.assertEqual(self.binary.read_bytes(), b"old executable")

    def test_failed_startup_restores_old_executable_without_rewinding_live_state(self):
        controller = FakeController(healthy=False)
        original_wait = controller.wait_ready

        def health(directory, version, job_id):
            if version == "0.7.1":
                self.state.write_bytes(b'{"new_receipt":"must survive rollback"}')
            return original_wait(directory, version, job_id)

        controller.wait_ready = health
        result = self.perform(controller)
        self.assertEqual(result["status"], "rolled_back")
        self.assertEqual(self.binary.read_bytes(), b"old executable")
        self.assertEqual(self.state.read_bytes(), b'{"new_receipt":"must survive rollback"}')

    def test_rollback_failure_is_not_reported_as_success(self):
        result = self.perform(FakeController(healthy=False, rollback_healthy=False))
        self.assertEqual(result["status"], "failed")
        self.assertIn("local attention", result["message"])

    def combined_request(self):
        self.home = self.root / "mdk"
        self.home.mkdir(mode=0o700)
        (self.home / "account.db").write_bytes(b"existing encrypted account and MLS state")
        self.info = {"version":"0.7.0", "state_version":1, "protocol":"v2",
                     "runtimes":["external-connector", "embedded-mdk"], "storage_cohort":"mdk-0.14",
                     "connector":{"version":"0.14.0", "revision":"11"*20, "patch_sha256":"22"*32}}
        self.request.update(info=self.info, messaging_home=str(self.home), service="codr")

    def test_combined_update_replaces_both_components_within_the_storage_cohort(self):
        self.combined_request()
        newer = {**self.info, "version":"0.7.1", "connector":{"version":"0.14.1", "revision":"33"*20, "patch_sha256":"44"*32}}
        controller = FakeController()
        result = self.perform(controller, newer)
        self.assertEqual(result["status"], "installed")
        self.assertEqual(self.binary.read_bytes(), self.payload)
        self.assertEqual((self.home/"account.db").read_bytes(), b"existing encrypted account and MLS state")

    def test_combined_update_refuses_storage_protocol_or_runtime_changes(self):
        self.combined_request()
        for mutate in [lambda x:x.update(storage_cohort="mdk-0.15"),
                       lambda x:x.update(protocol="v3"), lambda x:x.update(state_version=2),
                       lambda x:x.update(runtimes=["external-connector"]),
                       lambda x:x.update(connector={}),
                       lambda x:x["connector"].update(version="0.15.0")]:
            candidate = copy.deepcopy({**self.info,"version":"0.7.1"}); mutate(candidate)
            controller=FakeController(); result=self.perform(controller,candidate)
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("stop",controller.calls)

    def test_combined_rollback_preserves_new_messaging_state(self):
        self.combined_request()
        controller=FakeController(healthy=False)
        original_wait=controller.wait_ready
        def health(directory,version,job_id):
            if version=="0.7.1":(self.home/"account.db").write_bytes(b"new MLS traffic")
            return original_wait(directory,version,job_id)
        controller.wait_ready=health
        self.assertEqual(self.perform(controller)["status"],"rolled_back")
        self.assertEqual((self.home/"account.db").read_bytes(),b"new MLS traffic")
        self.assertEqual(self.binary.read_bytes(),b"old executable")

    def test_launchd_combined_service_validates_and_restarts_the_same_home(self):
        destination=self.root/".local/bin"/updater.WORKER; destination.parent.mkdir(parents=True)
        destination.write_bytes(b"combined binary")
        home=self.root/"mdk";home.mkdir()
        config=self.root/"config.toml"; config.write_text("configuration")
        plist=self.job/"combined.plist"
        args=[str(destination),"--config",str(config),"run","--messaging-home",str(home)]
        plist.write_bytes(plistlib.dumps({"ProgramArguments":args}))
        request={**self.request,"installed_path":str(destination),"base_sha256":updater.install.sha256(destination),
                 "pid":1234,"config_path":str(config),"messaging_home":str(home),"service":"codr"}
        selected=[]
        manager=SimpleNamespace(backend=updater.services.LAUNCHD,path=lambda name:(selected.append(name) or plist),
                                domain=lambda:"gui/123",start=lambda name:selected.append(name))
        with patch.object(Path,"home",return_value=self.root),patch.object(updater.services,"for_host",return_value=manager):
            controller=updater.Controller()
            with patch.object(controller,"pid",return_value=1234),patch.object(updater,"run",return_value="") as run:
                controller.validate(request);controller.stop();controller.start()
                self.assertEqual(run.call_args.args[0],["launchctl","bootout","gui/123/"+updater.services.LABEL_PREFIX+".codr"])
                self.assertEqual(selected,["codr","codr"])
                request["messaging_home"]=str(self.root/"another-home")
                with self.assertRaises(updater.UpdateError):controller.validate(request)

    def test_systemd_combined_service_targets_only_codr(self):
        manager=SimpleNamespace(backend=updater.services.SYSTEMD)
        with patch.object(updater.services,"for_host",return_value=manager),patch.object(updater,"run",return_value="1234") as run:
            controller=updater.Controller()
            controller.service="codr"
            self.assertEqual(controller.pid(),1234)
            self.assertIn("codr.service",run.call_args.args[0])
            controller.stop()
            self.assertEqual(run.call_args.args[0],["systemctl","--user","stop","codr.service"])

    def test_combined_runtime_can_keep_the_legacy_service_label_for_old_plugin_hooks(self):
        destination=self.root/".local/bin"/updater.WORKER;destination.parent.mkdir(parents=True)
        destination.write_bytes(b"combined executable")
        home=self.root/"mdk";home.mkdir()
        config=self.root/"config.toml";config.write_text("fixture")
        plist=self.root/"worker.plist"
        plist.write_bytes(plistlib.dumps({"ProgramArguments":[str(destination),"--config",str(config),"run","--messaging-home",str(home)]}))
        request={**self.request,"installed_path":str(destination),"base_sha256":updater.install.sha256(destination),
                 "pid":1234,"config_path":str(config),"messaging_home":str(home),"service":updater.WORKER}
        manager=SimpleNamespace(backend=updater.services.LAUNCHD,path=lambda name:plist,domain=lambda:"gui/123")
        with patch.object(Path,"home",return_value=self.root),patch.object(updater.services,"for_host",return_value=manager):
            controller=updater.Controller()
            with patch.object(controller,"pid",return_value=1234),patch.object(updater,"run",return_value="") as run:
                controller.validate(request)
                self.assertEqual(controller.service,updater.WORKER)
                self.assertTrue(controller.combined)
                controller.stop()
                self.assertTrue(run.call_args.args[0][-1].endswith("."+updater.WORKER))

    def test_combined_readiness_allows_slow_first_spawn_without_accepting_wrong_job(self):
        clock=[0.0]
        manager=SimpleNamespace(backend=updater.services.LAUNCHD,is_active=lambda name:None)
        (self.job/"ready.json").write_text(json.dumps({"version":"0.7.1","job_id":"job","pid":1234}))
        with patch.object(updater.services,"for_host",return_value=manager),patch.object(updater.time,"monotonic",side_effect=lambda:clock[0]),patch.object(updater.time,"sleep",side_effect=lambda seconds:clock.__setitem__(0,clock[0]+seconds)):
            controller=updater.Controller();controller.service="codr"
            with patch.object(controller,"pid",side_effect=lambda:1234 if clock[0]>=40 else 0):
                self.assertTrue(controller.wait_ready(self.job,"0.7.1","job"))
            self.assertGreaterEqual(clock[0],40)

    def test_combined_readiness_checks_only_the_selected_service(self):
        for backend in (updater.services.SYSTEMD, updater.services.LAUNCHD):
            with self.subTest(backend=backend):
                clock = [0.0]
                checked = []
                def active(name):
                    checked.append(name)
                    if name != "codr":
                        raise updater.UpdateError("Legacy service is disabled")
                manager = SimpleNamespace(backend=backend, is_active=active)
                (self.job/"ready.json").write_text(json.dumps({"version":"0.7.1","job_id":"job","pid":1234}))
                with patch.object(updater.services,"for_host",return_value=manager), patch.object(updater.time,"monotonic",side_effect=lambda:clock[0]), patch.object(updater.time,"sleep",side_effect=lambda seconds:clock.__setitem__(0,clock[0]+seconds)):
                    controller = updater.Controller()
                    controller.service = "codr"
                    with patch.object(controller,"pid",return_value=1234):
                        self.assertFalse(controller.wait_ready(self.job,"0.7.1","different-job"))
                        self.assertTrue(controller.wait_ready(self.job,"0.7.1","job"))
                self.assertEqual(checked,["codr"])

    def test_updater_runs_outside_the_bridge_service_on_linux_and_macos(self):
        (self.job / "request.json").write_text(json.dumps(self.request))
        for backend, command in [(updater.services.SYSTEMD, "systemd-run"), (updater.services.LAUNCHD, "launchctl")]:
            with self.subTest(backend=backend), patch.object(updater, "Controller", return_value=FakeController()), patch.object(updater.services, "find_python", return_value="/usr/bin/python3"), patch.object(updater.services, "detect_backend", return_value=backend), patch.object(updater, "run", return_value="") as run:
                updater.launch(self.job)
                argv = run.call_args.args[0]
                self.assertEqual(argv[0], command)
                self.assertIn("--", argv)
                self.assertEqual(argv[-4:], ["/usr/bin/python3", str(self.job / "update.py"), "install", str(self.job)])
                self.assertNotIn("sudo", argv)

    def test_launch_preflight_failure_is_distinguished_from_uncertain_submission(self):
        (self.job / "request.json").write_text(json.dumps(self.request))
        with patch.object(updater, "Controller") as controller, patch.object(updater, "run") as run:
            controller.return_value.validate.side_effect = updater.UpdateError("nonstandard install")
            with self.assertRaises(updater.NotStarted):
                updater.launch(self.job)
            run.assert_not_called()
        with patch.object(updater, "Controller", return_value=FakeController()), patch.object(updater.services, "find_python", return_value="/usr/bin/python3"), patch.object(updater.services, "detect_backend", return_value=updater.services.SYSTEMD), patch.object(updater, "run", side_effect=TimeoutError):
            with self.assertRaises(TimeoutError):
                updater.launch(self.job)

    def test_launchd_preflight_requires_the_requesting_process_not_just_a_matching_plist(self):
        destination = self.root / ".local/bin" / updater.WORKER
        destination.parent.mkdir(parents=True)
        destination.write_bytes(b"managed worker")
        plist = self.job / "worker.plist"
        plist.write_bytes(plistlib.dumps({"ProgramArguments": [str(destination), "run"]}))
        request = {**self.request, "installed_path": str(destination),
                   "base_sha256": updater.install.sha256(destination), "pid": 1234,
                   "config_path": str(self.root / ".config" / updater.WORKER / "config.toml")}
        manager = SimpleNamespace(backend=updater.services.LAUNCHD, path=lambda _: plist)
        with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(self.root / ".config")}), patch.object(Path, "home", return_value=self.root), patch.object(updater.services, "for_host", return_value=manager):
            controller = updater.Controller()
            for pid in [0, 9999]:
                with patch.object(controller, "pid", return_value=pid), self.assertRaises(updater.UpdateError):
                    controller.validate(request)
            with patch.object(controller, "pid", return_value=1234):
                controller.validate(request)

    def test_managed_config_symlink_matches_the_canonical_request_path(self):
        destination = self.root / ".local/bin" / updater.WORKER
        destination.parent.mkdir(parents=True)
        destination.write_bytes(b"managed worker")
        (self.root / "dotfiles").mkdir()
        (self.root / ".config").symlink_to(self.root / "dotfiles", target_is_directory=True)
        plist = self.job / "worker.plist"
        plist.write_bytes(plistlib.dumps({"ProgramArguments": [str(destination), "run"]}))
        config = self.root / ".config" / updater.WORKER / "config.toml"
        request = {**self.request, "installed_path": str(destination), "pid": 1234,
                   "base_sha256": updater.install.sha256(destination), "config_path": str(config.resolve())}
        manager = SimpleNamespace(backend=updater.services.LAUNCHD, path=lambda _: plist)
        with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(self.root / ".config")}), patch.object(Path, "home", return_value=self.root), patch.object(updater.services, "for_host", return_value=manager):
            controller = updater.Controller()
            with patch.object(controller, "pid", return_value=1234):
                controller.validate(request)


if __name__ == "__main__":
    unittest.main()
