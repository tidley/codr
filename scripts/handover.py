"""Owner-confirmed local service handover; never create or restore an account."""
from __future__ import annotations

import contextlib
import fcntl
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import signal
import socket
import stat
import tomllib
import uuid

import services
import update
from update import UpdateError, run

WORKER = update.WORKER
CONNECTOR = "wn-agent-herdr"
RELAYS = {"wss://relay.eu.whitenoise.chat", "wss://relay.us.whitenoise.chat"}


def private_write(path, text):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w") as output:
        output.write(text)
        output.flush()
        os.fsync(output.fileno())


@contextlib.contextmanager
def exclusive(path):
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def values(args, flag):
    result = []
    for index, value in enumerate(args):
        if value == flag:
            if index + 1 == len(args):
                raise UpdateError("Incomplete legacy service arguments; use local setup")
            result.append(args[index + 1])
    return result


class Plan:
    def __init__(self, request, controller):
        if request.get("action") != "handover" or request.get("messaging_home") is not None:
            raise UpdateError("This is not an external-to-combined handover")
        controller.validate(request)
        self.request, self.controller = request, controller
        self.manager = controller.manager
        self.manager.private_write = private_write
        self.binary = Path(request["installed_path"])
        self.config_path = Path(request["config_path"])
        self.config = tomllib.loads(self.config_path.read_text())
        self.state = Path(request["state_path"])
        if not self.state.is_absolute() or Path(self.config["state_path"]).resolve() != self.state.resolve():
            raise UpdateError("The configured routing state differs from this handover")
        info = update.probe(self.binary)
        if (info != request["info"] or "embedded-mdk" not in info.get("runtimes", [])
                or info.get("storage_cohort") != "mdk-0.14" or info.get("connector", {}).get("version") != "0.14.0"):
            raise UpdateError("This executable cannot hand over the existing messaging storage cohort")
        self.original_enabled = {}
        self.files = []
        self.overrides = []
        self.environment = {}
        self.original_pids = {}
        combined_path = self.service_path("codr")
        combined_overrides = combined_path.with_name("codr.service.d")
        if (self.pid("codr") or combined_path.exists() or combined_path.is_symlink()
                or (self.manager.backend == services.SYSTEMD and (combined_overrides.exists() or combined_overrides.is_symlink()))):
            raise UpdateError("A combined service already exists; inspect it locally")
        if self.manager.backend == services.SYSTEMD:
            runtime = services.runtime_dir()/"systemd/user"
            if (self.show("codr", "FragmentPath") or self.show("codr", "DropInPaths")
                    or (runtime/"codr.service").exists() or (runtime/"codr.service").is_symlink()
                    or (runtime/"codr.service.d").exists()):
                raise UpdateError("Another combined service definition needs local inspection")
        for name in (WORKER, CONNECTOR):
            pid = self.pid(name)
            if pid <= 0:
                raise UpdateError("Both legacy services must be healthy before an automatic handover")
            self.original_pids[name] = pid
            path = self.service_path(name)
            if not path.is_file() or path.is_symlink():
                raise UpdateError("The legacy service definition is unavailable")
            self.files.append(path)
            if self.manager.backend == services.SYSTEMD:
                allowed_directives = {"Description", "After", "Requires", "Wants", "Type", "ExecStart",
                                      "Environment", "Restart", "RestartSec", "UMask", "KillMode", "TimeoutStopSec", "WantedBy"}
                for line in path.read_text().splitlines():
                    line = line.strip()
                    if not line or line.startswith(("#", ";", "[")):
                        continue
                    key, separator, value = line.partition("=")
                    if not separator or key not in allowed_directives or (key == "Environment" and any(not word.startswith("PATH=") for word in shlex.split(value))):
                        raise UpdateError("Custom service configuration needs local setup")
                self.original_enabled[name] = self.show(name, "UnitFileState")
                if self.original_enabled[name] not in ("enabled", "disabled", "static"):
                    raise UpdateError("Custom service startup needs local setup")
                if name == WORKER and self.original_enabled[name] == "static":
                    raise UpdateError("A static worker service needs local setup")
                # Only known, non-secret overrides can move to the combined unit.
                roots = [self.manager.unit_root, services.runtime_dir()/"systemd/user"]
                for root in roots:
                    directory = root/(name+".service.d")
                    if directory.exists():
                        if directory.is_symlink():
                            raise UpdateError("Symlinked service overrides need local setup")
                        for override in directory.iterdir():
                            if override.is_symlink() or not override.is_file():
                                raise UpdateError("Unsafe service override")
                            lines = {line.strip() for line in override.read_text().splitlines() if line.strip() and not line.lstrip().startswith("#")}
                            if name == WORKER and lines <= {"[Unit]", "Wants=codr-phonon.service", "After=codr-phonon.service"}:
                                self.overrides.append(override)
                            elif name != WORKER or lines != {"[Service]", "KillMode=process"}:
                                raise UpdateError("Custom service overrides need local setup; nothing was stopped")
                            self.files.append(override)
                if Path(self.show(name, "FragmentPath")).resolve() != path.resolve():
                    raise UpdateError("The effective service fragment differs from its backup")
                inspected = {p.resolve() for p in self.files}
                if any(Path(p).resolve() not in inspected for p in shlex.split(self.show(name, "DropInPaths"))):
                    raise UpdateError("An unknown effective service override needs local setup")
            else:
                disabled = run(["launchctl", "print-disabled", self.manager.domain()])
                self.original_enabled[name] = not bool(re.search(r'"'+re.escape(services.LABEL_PREFIX+"."+name)+r'"\s*=>\s*true', disabled))
                payload = plistlib.loads(path.read_bytes())
                environment = payload.get("EnvironmentVariables", {})
                if name == CONNECTOR and set(environment) - {"PATH"}:
                    raise UpdateError("Custom connector environment needs local setup")
                if name == WORKER:
                    self.environment = environment
        worker_args = self.process_args(WORKER, request["pid"])
        expected = [str(self.binary), "run"]
        configured = [str(self.binary), "--config", str(self.config_path), "run"]
        if worker_args not in (expected, configured):
            raise UpdateError("Custom worker arguments need local setup")
        args = self.process_args(CONNECTOR, self.pid(CONNECTOR))
        self.connector_args = args
        if self.manager.backend == services.SYSTEMD:
            for name, actual in [(WORKER, worker_args), (CONNECTOR, args)]:
                starts = [line.partition("=")[2] for line in self.service_path(name).read_text().splitlines() if line.startswith("ExecStart=")]
                if len(starts) != 1:
                    raise UpdateError("Ambiguous service commands need local setup")
                disk_args = [value.replace("%h", str(Path.home())).replace("%%", "%").replace("$$", "$") for value in shlex.split(starts[0])]
                if disk_args != actual:
                    raise UpdateError("The saved service command differs from its running process")
        allowed = {"--home", "--socket", "--relay", "--media-allowed-root"}
        if not args or len(args) % 2 != 1 or any(args[i] not in allowed for i in range(1, len(args), 2)):
            raise UpdateError("Custom connector arguments need local setup")
        homes, sockets = values(args, "--home"), values(args, "--socket")
        if len(homes) != 1 or len(sockets) != 1 or not Path(homes[0]).is_absolute():
            raise UpdateError("The connector account home is ambiguous")
        self.home = Path(homes[0])
        self.socket = Path(sockets[0])
        self.connector = Path(args[0])
        if (self.home.is_symlink() or not self.home.is_dir() or not self.socket.is_relative_to(self.home)
                or self.socket.resolve() != Path(self.config["wn_agent_socket"]).resolve()
                or set(values(args, "--relay")) != RELAYS
                or any(Path(p) != self.state.with_suffix(".outbound-media") for p in values(args, "--media-allowed-root"))):
            raise UpdateError("The legacy connector layout differs from the combined runtime")
        if not self.connector.is_absolute() or self.connector.is_symlink() or run([self.connector, "--version"]).strip() != "wn-agent 0.14.0":
            raise UpdateError("The previous connector is outside the supported storage cohort")
        accounts = list((self.home/"accounts").glob("*/account.json"))
        if len(accounts) != 1 or accounts[0].is_symlink():
            raise UpdateError("The account home is ambiguous; use local setup")
        account = json.loads(accounts[0].read_text())
        secret = accounts[0].with_name("secret.json")
        if (account.get("account_id_hex") != self.config["account_id_hex"] or account.get("local_signing") is not True
                or not secret.is_file() or secret.is_symlink() or secret.stat().st_uid != os.getuid() or secret.stat().st_mode & 0o077):
            raise UpdateError("The existing signing identity or file-backed credentials cannot be verified")
        for source in (self.home, self.state.parent, self.config_path.parent):
            if source.is_symlink() or any(path.is_symlink() for path in source.rglob("*")):
                raise UpdateError("Symlinked persistent data needs local setup before handover")
        if not self.account_ready():
            raise UpdateError("The existing signing account is unavailable")
        self.connector_hash = update.install.sha256(self.connector)
        self.service_hashes = {path:update.install.sha256(path) for path in self.files}
        self.retired = self.service_path(WORKER).with_name(self.service_path(WORKER).name+".codr-retired")
        if self.retired.exists() or self.retired.is_symlink():
            raise UpdateError("An earlier handover needs local inspection")

    def original_connector_running(self):
        return (self.pid(CONNECTOR) == self.original_pids[CONNECTOR]
                and self.process_args(CONNECTOR, self.original_pids[CONNECTOR]) == self.connector_args
                and update.install.sha256(self.connector) == self.connector_hash and self.account_ready()
                and all(update.install.sha256(path) == digest for path,digest in self.service_hashes.items()))

    def show(self, name, field):
        return run(["systemctl", "--user", "show", name+".service", "--property="+field, "--value"]).strip()

    def pid(self, name):
        if self.manager.backend == services.SYSTEMD:
            return int(self.show(name, "MainPID") or "0")
        try:
            output = run(["launchctl", "print", self.manager.domain()+"/"+services.LABEL_PREFIX+"."+name])
        except Exception:
            return 0
        found = re.search(r"(?m)^\s*pid = (\d+)\s*$", output)
        return int(found[1]) if found else 0

    def service_path(self, name):
        return self.manager.unit_root/(name+".service") if self.manager.backend == services.SYSTEMD else self.manager.path(name)

    def process_args(self, name, pid):
        if self.manager.backend == services.SYSTEMD:
            args = Path(f"/proc/{pid}/cmdline").read_bytes().decode().strip("\0").split("\0")
            if Path(f"/proc/{pid}/exe").resolve() != Path(args[0]).resolve():
                raise UpdateError("The legacy process differs from its executable")
            return args
        args = plistlib.loads(self.service_path(name).read_bytes())["ProgramArguments"]
        if Path(run(["ps", "-p", str(pid), "-o", "comm="]).strip()).resolve() != Path(args[0]).resolve():
            raise UpdateError("The launchd process differs from its executable")
        return args

    def account_ready(self):
        request = {"id":str(uuid.uuid4()),"type":"account_list","marmot_agent_control":"marmot.agent-control.v2"}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
            control.settimeout(5)
            control.connect(str(self.socket))
            control.sendall(json.dumps(request).encode()+b"\n")
            with control.makefile("rb") as reader:
                response = json.loads(reader.readline(1024*1024))
        return (response.get("id") == request["id"] and response.get("marmot_agent_control") == request["marmot_agent_control"]
                and response.get("type") == "account_list" and any(a.get("account_id_hex") == self.config["account_id_hex"]
                and a.get("local_signing") is True for a in response.get("accounts", [])))

    def stop(self, name):
        if self.manager.backend == services.SYSTEMD:
            run(["systemctl", "--user", "stop", name+".service"], timeout=150)
        else:
            run(["launchctl", "bootout", self.manager.domain()+"/"+services.LABEL_PREFIX+"."+name])

    def enable(self, name, enabled):
        if self.manager.backend == services.SYSTEMD:
            if name != "codr" and self.original_enabled[name] == "static":
                return
            run(["systemctl", "--user", "enable" if enabled else "disable", name+".service"])
        else:
            run(["launchctl", "enable" if enabled else "disable", self.manager.domain()+"/"+services.LABEL_PREFIX+"."+name])

    @contextlib.contextmanager
    def stopped_home(self):
        if any(self.pid(name) for name in (WORKER, CONNECTOR, "codr")):
            raise UpdateError("A legacy or combined process has not stopped")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
            control.settimeout(1)
            try:
                control.connect(str(self.socket))
            except (FileNotFoundError, ConnectionRefusedError):
                pass
            else:
                raise UpdateError("A messaging socket owner is still running")
        with exclusive(self.state.with_suffix(".lock")), exclusive(self.home/".marmot-runtime.lock"):
            yield


def backup_tree(source, destination, hashes):
    if source.is_symlink():
        raise UpdateError("Symlinked persistent data needs a manual backup")
    if source.is_dir():
        destination.mkdir(mode=0o700)
        for path in source.iterdir():
            backup_tree(path, destination/path.name, hashes)
    elif stat.S_ISREG(source.stat().st_mode):
        shutil.copyfile(source, destination)
        destination.chmod(0o600)
        digest = update.install.sha256(source)
        if update.install.sha256(destination) != digest:
            raise UpdateError("The stopped backup checksum does not match")
        hashes[str(destination)] = digest
    elif not stat.S_ISSOCK(source.stat().st_mode):
        raise UpdateError("Unsupported persistent file in the account backup")


def perform(directory, request, controller):
    previous_handlers = {}
    def interrupted(number, frame):
        raise SystemExit(128 + number)
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            previous_handlers[number] = signal.getsignal(number)
            signal.signal(number, interrupted)
        with exclusive(Path(request["config_path"]).parent/"setup.lock"):
            return perform_locked(directory, request, controller)
    except Exception:
        return {"version":request["info"]["version"],"job_id":request["job_id"],"status":"failed",
                "message":"The combined handover could not complete. Inspect the private job before retrying. No account snapshot was restored."}
    finally:
        for number, handler in previous_handlers.items():
            signal.signal(number, handler)


def perform_locked(directory, request, controller):
    result = {"version":request["info"]["version"], "job_id":request["job_id"], "status":"failed",
              "message":"Combined handover refused before service changes; use local setup for this layout."}
    plan = None
    stopped = created = False
    backup = None
    try:
        plan = Plan(request, controller)
        job = json.loads(plan.state.read_text()).get("update_job")
        if job is not None and job.get("id") != request["job_id"]:
            raise UpdateError("Another update owns this routing state")
        backup = Path.home()/".local/state/codr/backups"/request["job_id"]
        if any(backup.is_relative_to(p) or p.is_relative_to(backup) for p in (plan.home, plan.state.parent, plan.config_path.parent)):
            raise UpdateError("A separate device-local backup location is required")
        backup.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        backup.mkdir(mode=0o700)
        stopped = True
        plan.stop(WORKER)
        plan.stop(CONNECTOR)
        with plan.stopped_home():
            hashes = {}
            for source, name in [(plan.home,"mdk-home"),(plan.state.parent,"bridge-state"),(plan.config_path.parent,"configuration")]:
                backup_tree(source, backup/name, hashes)
            for source, name in [(plan.state,"state.json"),(plan.config_path,"config.toml"),(plan.binary,"previous"),(plan.connector,"previous-connector")]:
                backup_tree(source, backup/name, hashes)
            (backup/"services").mkdir(mode=0o700)
            for index, source in enumerate(plan.files):
                backup_tree(source, backup/"services"/(str(index)+"-"+source.name), hashes)
            private_write(backup/"manifest.json", json.dumps({"sha256":hashes,"cold_backup":True}))
            os.sync()
            if any(update.install.sha256(path) != digest for path,digest in plan.service_hashes.items()):
                raise UpdateError("A service definition changed during its backup")
            for name in (WORKER, CONNECTOR):
                plan.enable(name, False)
            combined = services.combined_service(plan.binary, plan.config_path, plan.home)
            # Keep the old label: released plugin hooks still start this name.
            combined.name = WORKER
            combined.environment.update(plan.environment)
            combined.environment["CODR_SERVICE_NAME"] = WORKER
            created = True
            plan.service_path(WORKER).rename(plan.retired)
            if plan.manager.backend == services.SYSTEMD:
                template = (directory/"codr.service").read_text().replace("[Service]\n", "[Service]\nEnvironment=CODR_SERVICE_NAME="+WORKER+"\n")
                plan.manager.install(combined, template=template)
            else:
                plan.manager.install(combined)
            plan.manager.reload()
        controller.service = WORKER
        controller.combined = True
        plan.enable(WORKER, True)
        plan.manager.start(WORKER)
        if not controller.wait_ready(directory, result["version"], request["job_id"]) or not plan.account_ready():
            raise UpdateError("Combined messaging did not confirm startup")
        result.update(status="installed", backup=str(backup), message="Combined upgrade complete. Bridge and messaging now run together. Accounts, chats and accepted work were kept. Private cold backup: "+str(backup))
    except BaseException:
        if stopped and plan is not None:
            for number in (signal.SIGINT, signal.SIGTERM):
                signal.signal(number, signal.SIG_IGN)
            errors = []
            def attempt(action):
                try:
                    action()
                    return True
                except Exception:
                    errors.append(True)
                    return False
            safe = True
            if created:
                attempt(lambda: plan.stop(WORKER))
                attempt(lambda: plan.enable(WORKER, False))
            try:
                if not created and plan.original_connector_running():
                    # A failed connector stop must not leave the original chat
                    # bridge down while that same healthy connector still runs.
                    safe = plan.pid(WORKER) in (0, plan.original_pids[WORKER])
                else:
                    with plan.stopped_home():
                        if created:
                            path = plan.service_path(WORKER)
                            if plan.retired.exists():
                                if path.exists():
                                    path.rename(backup/"failed-combined-service")
                                plan.retired.rename(path)
                            elif not path.exists() or update.install.sha256(path) != plan.service_hashes[path]:
                                raise UpdateError("The previous service definition cannot be verified")
                            plan.manager.reload()
            except Exception:
                safe = False
            if safe:
                for name in (CONNECTOR, WORKER):
                    enabled = plan.original_enabled[name]
                    attempt(lambda name=name, enabled=enabled: plan.enable(name, enabled is True or enabled == "enabled"))
                    if not plan.pid(name):
                        attempt(lambda name=name: plan.manager.start(name))
                controller.service = WORKER
                controller.combined = False
                healthy = False
                if not errors:
                    try:
                        healthy = controller.wait_ready(directory, result["version"], request["job_id"])
                    except Exception:
                        pass
                if healthy:
                    result.update(status="rolled_back", message="Combined handover failed; the previous two services are running. Live account and chat state were not rewound.")
                else:
                    result["message"] = "Combined handover failed and recovery needs local attention. Account state was not rewound. No automatic retry will run."
            else:
                result["message"] = "Combined handover stopped because account ownership or service recovery could not be verified. Local attention is required; no account snapshot was restored."
    return result
