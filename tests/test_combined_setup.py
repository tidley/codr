"""Fixture-only handover checks: no real services or accounts are used."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location("combined_setup_fixture",Path(__file__).resolve().parents[1]/"scripts/setup.py")
setup=importlib.util.module_from_spec(spec);spec.loader.exec_module(setup)


class CombinedSetupTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.home=self.root/"mdk";self.home.mkdir(mode=0o700)
        (self.home/"account.db").write_bytes(b"existing encrypted account and MLS state")
        (self.home/"codr-bootstrap.json").write_text(json.dumps({"account_id_hex":"ab"*32}))
        self.state=self.root/"state.json";self.state.write_text('{"accepted":["keep"]}')
        self.config=self.root/"config.toml";self.config.write_text(setup.toml_text({"account_id_hex":"ab"*32,"state_path":self.state,"wn_agent_socket":self.home/"dev/wn.sock"}))
        self.binary=self.root/".local/bin/herdr-white-noise-bridge";self.binary.parent.mkdir(parents=True);self.binary.write_text("combined")
        self.connector=self.root/"old-connector";self.connector.write_text("previous connector")
        self.units=self.root/"units";self.units.mkdir()
        for name in ["herdr-white-noise-bridge","wn-agent-herdr"]:(self.units/(name+".service")).write_text("existing "+name)
        self.backup=self.root/"backup";self.backup.mkdir(mode=0o700)
        shutil.copytree(self.home,self.backup/"mdk-home")
        shutil.copy2(self.config,self.backup/"config.toml");shutil.copy2(self.state,self.backup/"state.json")
        shutil.copytree(self.units,self.backup/"services");(self.backup/"previous").write_text("old executable")
        self.calls=[];self.actions=[];self.active=False;self.enabled=False
        self.accounts=[{"account_id_hex":"ab"*32,"local_signing":True}]
        self.manager=SimpleNamespace(backend=setup.services.SYSTEMD,unit_root=self.units,
            install=lambda service:self.actions.append(("install",service)),reload=lambda:self.actions.append(("reload",)),
            start=lambda name:self.actions.append(("start",name)),is_active=lambda names:self.actions.append(("active",names)))

    def runner(self,args):
        self.calls.append([str(a) for a in args])
        if "update-info" in args:return json.dumps({"runtimes":["external-connector","embedded-mdk"],"storage_cohort":"mdk-0.14","connector":{"version":"0.14.0"}})
        if "--version" in args:return "wn-agent 0.14.0"
        if "--property=MainPID" in args:return "123" if self.active else "0"
        if "--property=ActiveState" in args:return "active" if self.active else "inactive"
        if "--property=UnitFileState" in args:return "enabled" if self.enabled else "disabled"
        return ""

    def invoke(self,approved=True):
        with patch.object(Path,"home",return_value=self.root),patch.object(setup.services,"for_host",return_value=self.manager),patch.object(setup,"local_accounts",return_value=self.accounts):
            return setup.configure_combined(self.config,self.home,self.backup,self.connector,approved,runner=self.runner)

    def test_handover_installs_one_service_without_changing_account_state_or_config(self):
        oldconfig=self.config.read_bytes();oldstate=self.state.read_bytes()
        report=self.invoke()
        installed=[a for a in self.actions if a[0]=="install"];self.assertEqual(len(installed),1)
        service=installed[0][1];self.assertEqual(service.name,"codr")
        self.assertEqual(service.argv,[str(self.binary),"--config",str(self.config),"run","--messaging-home",str(self.home)])
        self.assertIn(("start","codr"),self.actions)
        self.assertEqual(self.state.read_bytes(),oldstate);self.assertEqual(self.config.read_bytes(),oldconfig)
        self.assertEqual((self.home/"account.db").read_bytes(),b"existing encrypted account and MLS state")
        self.assertEqual(report["service"],"codr")
        self.assertFalse(any("stop" in c or "restart" in c for c in self.calls))

    def test_handover_requires_approval_and_stopped_disabled_legacy_services(self):
        with self.assertRaises(setup.SetupError):self.invoke(False)
        self.assertFalse(self.calls);self.assertFalse(self.actions)
        self.active=True
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)
        self.active=False;self.enabled=True
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_mismatched_private_backup_or_account_identity_refuses_before_install(self):
        (self.backup/"mdk-home/account.db").write_bytes(b"wrong snapshot")
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)
        shutil.copy2(self.home/"account.db",self.backup/"mdk-home/account.db")
        (self.home/"codr-bootstrap.json").write_text(json.dumps({"account_id_hex":"ff"*32}))
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_a_pending_independent_updater_blocks_the_first_service_handover(self):
        self.state.write_text(json.dumps({"update_job":{"id":"old-job"}}))
        shutil.copy2(self.state,self.backup/"state.json")
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_unsafe_backup_or_different_connector_cohort_is_refused(self):
        self.backup.chmod(0o755)
        with self.assertRaises(setup.SetupError):self.invoke()
        self.backup.chmod(0o700)
        original=self.runner
        with patch.object(self,"runner",side_effect=lambda a:"wn-agent 0.13.0" if "--version" in a else original(a)):
            with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_backup_cannot_be_a_symlink_to_live_account_storage(self):
        shutil.rmtree(self.backup/"mdk-home")
        (self.backup/"mdk-home").symlink_to(self.home,target_is_directory=True)
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_unmanaged_control_socket_owner_refuses_before_service_install(self):
        sock=self.home/"dev/wn.sock";sock.parent.mkdir()
        with setup.socket.socket(setup.socket.AF_UNIX,setup.socket.SOCK_STREAM) as listener:
            listener.bind(str(sock));listener.listen(1)
            with self.assertRaises(setup.SetupError):self.invoke()
        self.assertFalse(self.actions)

    def test_bad_messaging_health_stops_only_the_combined_service_without_restore(self):
        self.accounts=[]
        before=self.state.read_bytes()
        with self.assertRaises(setup.SetupError):self.invoke()
        self.assertIn(["systemctl","--user","stop","codr.service"],self.calls)
        self.assertEqual(self.state.read_bytes(),before)
        self.assertEqual((self.home/"account.db").read_bytes(),b"existing encrypted account and MLS state")

    def test_launchd_handover_requires_bootout_and_disabled_old_jobs(self):
        self.manager.backend=setup.services.LAUNCHD
        self.manager.domain=lambda:"gui/123"
        self.manager.loaded=lambda name:False
        self.manager.path=lambda name:self.units/(name+".plist")
        for name in ["herdr-white-noise-bridge","wn-agent-herdr"]:
            p=self.manager.path(name);p.write_text("old plist");shutil.copy2(p,self.backup/"services"/p.name)
        original=self.runner
        disabled='"io.github.tidley.codr.herdr-white-noise-bridge" => true\n"io.github.tidley.codr.wn-agent-herdr" => true'
        with patch.object(self,"runner",side_effect=lambda a:disabled if "print-disabled" in a else original(a)):
            self.invoke()
        self.assertIn(("start","codr"),self.actions)

    def test_launchd_failed_handover_can_retry_after_health_is_repaired(self):
        self.manager.backend=setup.services.LAUNCHD
        self.manager.domain=lambda:"gui/123"
        self.manager.loaded=lambda name:False
        self.manager.path=lambda name:self.units/(name+".plist")
        disabled='"io.github.tidley.codr.herdr-white-noise-bridge" => true\n"io.github.tidley.codr.wn-agent-herdr" => true'
        original=self.runner
        combined_disabled=[False]
        def runner(args):
            if "print-disabled" in args:
                return disabled
            if args[:2]==["launchctl","disable"]:
                combined_disabled[0]=True
            if args[:2]==["launchctl","enable"]:
                combined_disabled[0]=False
            return original(args)
        def start(name):
            if combined_disabled[0]:
                raise setup.SetupError("Combined launchd label remains disabled")
            self.actions.append(("start",name))
        self.manager.start=start
        with patch.object(self,"runner",side_effect=runner):
            self.accounts=[]
            with self.assertRaises(setup.SetupError):self.invoke()
            self.assertTrue(combined_disabled[0])
            self.accounts=[{"account_id_hex":"ab"*32,"local_signing":True}]
            self.assertEqual(self.invoke()["service"],"codr")
        self.assertFalse(combined_disabled[0])
