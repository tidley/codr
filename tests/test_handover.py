"""All service, account and credential fixtures are private temporary files."""
import contextlib
import copy
import fcntl
import importlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import socket
import threading
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import services
import update


class HandoverTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue((Path(services.ROOT) / "scripts/handover.py").is_file(), "Automatic handover helper is missing")
        self.handover = importlib.import_module("handover")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / ".config/herdr-white-noise-bridge/config.toml"
        self.config.parent.mkdir(parents=True)
        self.home = self.root / ".marmot-agents/herdr"
        self.account = self.home / "accounts/bridge"
        self.account.mkdir(parents=True)
        (self.account / "account.json").write_text(json.dumps({"account_id_hex":"ab"*32,"local_signing":True}))
        (self.account / "secret.json").write_bytes(b"fixture credential; never decode")
        (self.account / "secret.json").chmod(0o600)
        (self.account / "session.sqlite").write_bytes(b"existing MLS state")
        (self.account / "session.sqlite-wal").write_bytes(b"existing sidecar")
        (self.home / ".marmot-runtime.lock").touch()
        self.state = self.root / "state/bridge/state.json"
        self.state.parent.mkdir(parents=True)
        self.state.write_text(json.dumps({"mappings":{"chat":{"pending_prompt":True}},"update_job":{"id":"job"}}))
        self.state.with_suffix(".lock").touch()
        self.config.write_text('\n'.join(f'{k} = {json.dumps(str(v))}' for k,v in {
            "account_id_hex":"ab"*32,"state_path":self.state,"wn_agent_socket":self.home/"dev/control.sock"}.items()))
        self.binary = self.root / ".local/bin/herdr-white-noise-bridge"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"installed 0.10 executable")
        self.connector = self.root / "connector"
        self.connector.write_bytes(b"old external MDK")
        self.job = self.root / "state/bridge/state.updates/job"
        self.job.mkdir(parents=True, mode=0o700)
        shutil.copyfile(Path(services.ROOT)/"systemd/codr.service",self.job/"codr.service")
        self.info = {"version":"0.10.0","state_version":1,"protocol":"marmot.agent-control.v2",
                     "runtimes":["external-connector","embedded-mdk"],"storage_cohort":"mdk-0.14",
                     "connector":{"version":"0.14.0","revision":"11"*20,"patch_sha256":"22"*32}}
        self.request = {"action":"handover","job_id":"job","info":self.info,"pid":123,
                        "installed_path":str(self.binary),"base_sha256":update.install.sha256(self.binary),
                        "config_path":str(self.config),"state_path":str(self.state),"service":update.WORKER,"messaging_home":None}
        self.active = {update.WORKER:123,"wn-agent-herdr":456,"codr":0}
        self.enabled = {update.WORKER:"enabled","wn-agent-herdr":"static","codr":"disabled"}
        self.calls = []
        self.healthy = True
        self.health_callback = None
        self.manager = services.SystemdServices(self.runner, self.handover.private_write, self.root/".config")
        self.manager.unit_root.mkdir(parents=True)
        for name in [update.WORKER,"wn-agent-herdr"]:
            args=" ".join(services.systemd_argument(arg) for arg in self.args(name,0))
            (self.manager.unit_root/(name+".service")).write_text("[Unit]\nDescription=fixture\n[Service]\nType=simple\nExecStart="+args+"\nRestart=on-failure\n[Install]\nWantedBy=default.target\n")
        self.controller = type("FixtureController", (), {})()
        self.controller.manager = self.manager
        self.controller.service = update.WORKER
        self.controller.combined = False
        self.controller.validate = lambda request: self.calls.append(("validate",))
        def wait_ready(directory, version, job_id):
            if self.health_callback:
                self.health_callback("codr" if self.controller.combined else update.WORKER)
            return self.healthy if self.controller.combined else True
        self.controller.wait_ready = wait_ready

    def args(self, name, pid):
        if name == update.WORKER:
            return [str(self.binary),"--config",str(self.config),"run"]
        return [str(self.connector),"--home",str(self.home),"--socket",str(self.home/"dev/control.sock"),
                "--relay","wss://relay.eu.whitenoise.chat","--relay","wss://relay.us.whitenoise.chat"]

    def runner(self, args, **kwargs):
        args = [str(a) for a in args]
        self.calls.append(tuple(args))
        if "--version" in args:
            return "wn-agent 0.14.0"
        if args[:2] == ["systemctl","--user"]:
            action=args[2]
            name=next((a.removesuffix(".service") for a in args[3:] if a.endswith(".service")),None)
            if action=="show":
                if "--property=MainPID" in args:return str(self.active.get(name,0))
                if "--property=ActiveState" in args:return "active" if self.active.get(name) else "inactive"
                if "--property=UnitFileState" in args:return self.enabled.get(name,"disabled")
                if "--property=DropInPaths" in args:return ""
                if "--property=FragmentPath" in args:
                    path=self.manager.unit_root/(name+".service")
                    return str(path) if path.exists() else ""
            if action=="stop":self.active[name]=0
            if action=="start":self.active[name]=789
            if action=="enable":self.enabled[name]="enabled"
            if action=="disable":self.enabled[name]="disabled"
        if args[:2]==["launchctl","print"]:
            name=args[-1].rsplit(".",1)[-1]
            return f"state = running\npid = {self.active[name]}" if self.active.get(name) else ""
        if args[:2]==["launchctl","print-disabled"]:return "{}"
        if args[:2]==["launchctl","bootout"]:self.active[args[-1].rsplit(".",1)[-1]]=0
        if args[:2]==["launchctl","bootstrap"]:
            name=plistlib.loads(Path(args[-1]).read_bytes())["Label"].rsplit(".",1)[-1];self.active[name]=789
        return ""

    def perform(self):
        with patch.dict(os.environ,{"XDG_RUNTIME_DIR":str(self.root/"run")}),patch.object(Path,"home",return_value=self.root),patch.object(self.handover,"run",side_effect=self.runner),patch.object(update,"probe",return_value=self.info),patch.object(self.handover.Plan,"process_args",side_effect=self.args),patch.object(self.handover.Plan,"account_ready",return_value=True):
            return self.handover.perform(self.job,self.request,self.controller)

    def test_systemd_handover_cold_backup_preserves_credentials_state_and_configuration(self):
        state=self.state.read_bytes();config=self.config.read_bytes()
        result=self.perform()
        self.assertEqual(result["status"],"installed")
        backup=Path(result["backup"])
        self.assertEqual((backup/"mdk-home/accounts/bridge/secret.json").read_bytes(),b"fixture credential; never decode")
        self.assertEqual((backup/"mdk-home/accounts/bridge/session.sqlite-wal").read_bytes(),b"existing sidecar")
        self.assertEqual((backup/"state.json").read_bytes(),state)
        self.assertEqual(self.config.read_bytes(),config)
        self.assertEqual(self.state.read_bytes(),state)
        self.assertGreater(self.active[update.WORKER],0);self.assertEqual(self.active["wn-agent-herdr"],0)
        self.assertEqual(self.active["codr"],0)
        self.assertEqual(self.enabled[update.WORKER],"enabled")
        self.assertFalse(any(p.stat().st_mode&0o077 for p in [backup,*backup.rglob("*")]))
        unit=(self.manager.unit_root/(update.WORKER+".service")).read_text()
        self.assertIn("--messaging-home",unit)
        self.assertIn("CODR_SERVICE_NAME="+update.WORKER,unit)

    def test_bad_health_rolls_back_services_without_rewinding_new_mls_state(self):
        self.healthy=False
        self.health_callback=lambda service:(self.account/"session.sqlite").write_bytes(b"new traffic") if service=="codr" else None
        result=self.perform()
        self.assertEqual(result["status"],"rolled_back")
        self.assertEqual((self.account/"session.sqlite").read_bytes(),b"new traffic")
        self.assertFalse((self.manager.unit_root/"codr.service").exists())
        self.assertGreater(self.active[update.WORKER],0)
        self.assertEqual(self.enabled[update.WORKER],"enabled")

    def test_recovery_keeps_the_setup_lock_until_legacy_restart_is_verified(self):
        self.healthy=False
        held=[]
        def health(service):
            if service != update.WORKER:return
            with (self.config.parent/"setup.lock").open("r+") as guard:
                try:fcntl.flock(guard,fcntl.LOCK_EX|fcntl.LOCK_NB)
                except BlockingIOError:held.append(True)
                else:held.append(False)
        self.health_callback=health
        self.assertEqual(self.perform()["status"],"rolled_back")
        self.assertEqual(held,[True])

    def test_termination_enters_recovery_and_restores_the_original_signal_handler(self):
        original=signal.getsignal(signal.SIGTERM)
        seen=[]
        def health(service):
            if service=="codr":
                handler=signal.getsignal(signal.SIGTERM)
                seen.append(callable(handler))
                if callable(handler):handler(signal.SIGTERM,None)
                else:raise KeyboardInterrupt()
        self.health_callback=health
        self.assertEqual(self.perform()["status"],"rolled_back")
        self.assertEqual(seen,[True],"SIGTERM must enter recovery")
        self.assertEqual(signal.getsignal(signal.SIGTERM),original)

    def test_existing_combined_overrides_are_not_overwritten_or_quarantined(self):
        directory=self.manager.unit_root/"codr.service.d";directory.mkdir()
        (directory/"custom.conf").write_text("user-owned override")
        self.assertEqual(self.perform()["status"],"failed")
        self.assertEqual((directory/"custom.conf").read_text(),"user-owned override")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_symlinked_account_data_or_custom_environment_refuses_before_stop(self):
        (self.home/"linked-data").symlink_to(self.connector)
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))
        (self.home/"linked-data").unlink()
        unit=self.manager.unit_root/(update.WORKER+".service")
        unit.write_text(unit.read_text()+"\nEnvironmentFile=/private/custom-environment\n")
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_unsafe_or_missing_credentials_or_account_mismatch_refuses_before_stop(self):
        for mode in ["missing","symlink","account"]:
            with self.subTest(mode=mode):
                secret=self.account/"secret.json"
                if mode=="missing":secret.unlink()
                elif mode=="symlink":secret.symlink_to(self.connector)
                else:
                    secret.unlink();secret.write_bytes(b"fixture");secret.chmod(0o600)
                    (self.account/"account.json").write_text(json.dumps({"account_id_hex":"ff"*32,"local_signing":True}))
                result=self.perform()
                self.assertEqual(result["status"],"failed")
                self.assertFalse(any("stop" in c for c in self.calls))

    def test_other_setup_lock_or_conflicting_combined_unit_refuses_before_stop(self):
        lock=self.config.parent/"setup.lock"
        with lock.open("w") as guard:
            fcntl.flock(guard,fcntl.LOCK_EX|fcntl.LOCK_NB)
            self.assertEqual(self.perform()["status"],"failed")
        (self.manager.unit_root/"codr.service").write_text("someone else's unit")
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_non_handover_request_cannot_bypass_same_version_release_checks(self):
        self.request["action"]="install"
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_effective_system_level_overrides_and_changed_unit_commands_refuse_before_stop(self):
        original=self.runner
        def runner(args,**kwargs):
            if "--property=DropInPaths" in args and update.WORKER+".service" in args:return "/etc/systemd/user/worker.service.d/custom.conf"
            return original(args,**kwargs)
        with patch.object(self,"runner",side_effect=runner):
            self.assertEqual(self.perform()["status"],"failed")
        unit=self.manager.unit_root/(update.WORKER+".service")
        unit.write_text(unit.read_text().replace("ExecStart=", "ExecStart=/bin/false # "))
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_runtime_combined_definition_refuses_before_stop(self):
        directory=self.root/"run/systemd/user/codr.service.d"
        directory.mkdir(parents=True)
        (directory/"custom.conf").write_text("[Service]\nExecStart=/unrelated\n")
        self.assertEqual(self.perform()["status"],"failed")
        self.assertFalse(any("stop" in c for c in self.calls))

    def test_corrupt_backup_or_partial_service_write_restores_legacy_layout(self):
        original_copy=shutil.copyfile
        def corrupt(source,destination,*args,**kwargs):
            result=original_copy(source,destination,*args,**kwargs)
            if Path(destination).name=="session.sqlite":Path(destination).write_bytes(b"corrupt copy")
            return result
        with patch.object(self.handover.shutil,"copyfile",side_effect=corrupt):
            self.assertEqual(self.perform()["status"],"rolled_back")
        self.assertEqual((self.account/"session.sqlite").read_bytes(),b"existing MLS state")
        # Use another job so the intentionally retained failed backup is not reused.
        self.job=self.job.with_name("job2");self.job.mkdir(mode=0o700)
        shutil.copyfile(Path(services.ROOT)/"systemd/codr.service",self.job/"codr.service")
        self.request["job_id"]="job2"
        self.request["pid"]=self.active[update.WORKER]
        self.state.write_text(json.dumps({"mappings":{},"update_job":{"id":"job2"}}))
        original_install=self.manager.install
        def partial(service,**kwargs):
            original_install(service,**kwargs)
            raise OSError("fixture failure after unit creation")
        with patch.object(self.manager,"install",side_effect=partial):
            self.assertEqual(self.perform()["status"],"rolled_back")
        self.assertNotIn("--messaging-home",(self.manager.unit_root/(update.WORKER+".service")).read_text())

    def test_account_health_validates_actual_socket_response_identity_and_protocol(self):
        for mismatch in [None,"id","protocol","account","unsigned"]:
            with self.subTest(mismatch=mismatch):
                path=self.root/("health-"+str(mismatch)+".sock")
                plan=object.__new__(self.handover.Plan)
                plan.socket=path;plan.config={"account_id_hex":"ab"*32}
                with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as listener:
                    listener.bind(str(path));listener.listen(1);listener.settimeout(5)
                    def serve():
                        connection,_=listener.accept()
                        with connection,connection.makefile("rb") as reader:
                            req=json.loads(reader.readline())
                            response={"id":req["id"],"type":"account_list","marmot_agent_control":"marmot.agent-control.v2",
                                      "accounts":[{"account_id_hex":"ab"*32,"local_signing":True}]}
                            if mismatch=="id":response["id"]="other"
                            if mismatch=="protocol":response["marmot_agent_control"]="v1"
                            if mismatch=="account":response["accounts"][0]["account_id_hex"]="ff"*32
                            if mismatch=="unsigned":response["accounts"][0]["local_signing"]=False
                            connection.sendall(json.dumps(response).encode()+b"\n")
                    thread=threading.Thread(target=serve);thread.start()
                    self.assertEqual(plan.account_ready(),mismatch is None)
                    thread.join(5);self.assertFalse(thread.is_alive())

    def test_partial_connector_stop_failure_restores_the_original_worker(self):
        original=self.runner
        def runner(args,**kwargs):
            if list(args[:3])==["systemctl","--user","stop"] and args[-1]=="wn-agent-herdr.service":
                raise OSError("fixture stop failure; original connector still healthy")
            return original(args,**kwargs)
        with patch.object(self,"runner",side_effect=runner):
            self.assertEqual(self.perform()["status"],"rolled_back")
        self.assertGreater(self.active[update.WORKER],0)
        self.assertEqual(self.active["wn-agent-herdr"],456)

    def test_changed_connector_during_partial_stop_is_not_trusted_for_recovery(self):
        original=self.runner
        def runner(args,**kwargs):
            if list(args[:3])==["systemctl","--user","stop"] and args[-1]=="wn-agent-herdr.service":
                self.active["wn-agent-herdr"]=999
                raise OSError("another connector replaced the original")
            return original(args,**kwargs)
        with patch.object(self,"runner",side_effect=runner):
            result=self.perform()
        self.assertEqual(result["status"],"failed")
        self.assertEqual(self.active[update.WORKER],0)
        self.assertEqual(self.active["wn-agent-herdr"],999)

    def test_storage_lock_contention_prevents_service_conversion(self):
        with self.state.with_suffix(".lock").open("r+") as guard:
            fcntl.flock(guard,fcntl.LOCK_EX|fcntl.LOCK_NB)
            result=self.perform()
        self.assertEqual(result["status"],"failed")
        self.assertNotIn("--messaging-home",(self.manager.unit_root/(update.WORKER+".service")).read_text())
        self.assertEqual((self.account/"session.sqlite").read_bytes(),b"existing MLS state")

    def test_socket_owner_remaining_after_stop_prevents_conversion(self):
        path=self.home/"dev/control.sock";path.parent.mkdir()
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as listener:
            listener.bind(str(path));listener.listen(2)
            result=self.perform()
        self.assertEqual(result["status"],"failed")
        self.assertNotIn("--messaging-home",(self.manager.unit_root/(update.WORKER+".service")).read_text())

    def test_failed_combined_stop_does_not_start_a_second_messaging_runtime(self):
        self.healthy=False
        original=self.runner
        def runner(args,**kwargs):
            if (list(args[:3])==["systemctl","--user","stop"] and args[-1]==update.WORKER+".service"
                    and self.controller.combined):
                raise OSError("fixture combined process could not stop")
            return original(args,**kwargs)
        with patch.object(self,"runner",side_effect=runner):
            result=self.perform()
        self.assertEqual(result["status"],"failed")
        self.assertEqual(self.active["wn-agent-herdr"],0)
        self.assertIn("--messaging-home",(self.manager.unit_root/(update.WORKER+".service")).read_text())

    def test_old_plugin_start_name_remains_the_single_combined_service(self):
        self.assertEqual(self.perform()["status"],"installed")
        old_path=self.manager.unit_root/(update.WORKER+".service")
        self.assertIn("--messaging-home",old_path.read_text())
        self.assertFalse((self.manager.unit_root/"codr.service").exists())
        self.assertEqual(self.controller.service,update.WORKER)

    def test_launchd_handover_uses_one_plist_and_disables_legacy_startup(self):
        self.use_launchd()
        with patch.object(services,"_capture",side_effect=self.runner):
            result=self.perform()
        self.assertEqual(result["status"],"installed")
        self.assertFalse(self.manager.path("codr").exists())
        payload=plistlib.loads(self.manager.path(update.WORKER).read_bytes())
        self.assertIn("--messaging-home",payload["ProgramArguments"])
        self.assertEqual(payload["EnvironmentVariables"]["CODR_SERVICE_NAME"],update.WORKER)
        self.assertTrue(any(c[:2]==("launchctl","disable") and c[-1].endswith(".wn-agent-herdr") for c in self.calls))

    def use_launchd(self):
        self.manager=services.LaunchdServices(self.runner,self.handover.private_write,self.root/".config")
        self.manager._domain="gui/123";self.manager.agents=self.root/"LaunchAgents";self.manager.agents.mkdir()
        for name in [update.WORKER,"wn-agent-herdr"]:
            service=services.Service(name,"fixture",self.args(name,0))
            self.manager.path(name).write_bytes(plistlib.dumps(self.manager.payload(service)))
        self.controller.manager=self.manager

    def test_launchd_bad_health_restores_original_plists_and_live_account(self):
        self.use_launchd()
        original=self.manager.path(update.WORKER).read_bytes()
        self.healthy=False
        with patch.object(services,"_capture",side_effect=self.runner):
            result=self.perform()
        self.assertEqual(result["status"],"rolled_back")
        self.assertEqual(self.manager.path(update.WORKER).read_bytes(),original)
        self.assertGreater(self.active[update.WORKER],0)
        self.assertGreater(self.active["wn-agent-herdr"],0)
        self.assertFalse(self.manager.path("codr").exists())
