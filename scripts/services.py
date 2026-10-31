"""Install and control codr's per-user services with systemd or launchd."""

from __future__ import annotations

import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
SYSTEMD = "systemd"
LAUNCHD = "launchd"
LABEL_PREFIX = "io.github.tidley.codr"


class ServiceError(Exception):
    pass


class Service:
    """One long-running process owned by the user's service manager."""

    def __init__(self, name, description, argv, environment=None, throttle=5):
        self.name = str(name)
        self.description = str(description)
        self.argv = [str(arg) for arg in argv]
        self.environment = dict(environment or {})
        self.throttle = int(throttle)

    @property
    def label(self):
        return f"{LABEL_PREFIX}.{self.name}"


def detect_backend():
    selected = os.environ.get("CODR_SERVICE_MANAGER")
    if selected in (SYSTEMD, LAUNCHD):
        return selected
    return LAUNCHD if sys.platform == "darwin" else SYSTEMD


def combined_service(binary, config, messaging_home):
    """Describe one process; installation/start and account handover stay explicit."""
    paths = [Path(value) for value in (binary, config, messaging_home)]
    if not all(path.is_absolute() for path in paths):
        raise ServiceError("Combined service paths must be absolute")
    return Service("codr", "codr bridge and MDK messaging runtime",
                   [paths[0], "--config", paths[1], "run", "--messaging-home", paths[2]],
                   environment={"PATH": worker_path()})


def for_host(runner, private_write, config_root, error=ServiceError):
    if detect_backend() == LAUNCHD:
        return LaunchdServices(runner, private_write, config_root, error)
    return SystemdServices(runner, private_write, config_root, error)


def worker_path():
    """PATH for supervised processes; launchd starts them without the user's shell PATH."""
    home = Path.home()
    return os.pathsep.join([str(home / ".local/bin"), str(home / ".opencode/bin"),
                            "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])


def runtime_dir():
    """Private directory for Unix sockets. systemd creates it on Linux; macOS needs an equivalent."""
    configured = os.environ.get("XDG_RUNTIME_DIR")
    if configured:
        root = Path(configured)
    elif sys.platform == "darwin":
        root = Path.home() / "Library/Application Support/codr/run"
    else:
        root = Path(f"/run/user/{os.getuid()}")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    return root


def check_socket_path(path):
    """macOS stores Unix socket paths in a 104-byte sun_path field."""
    if sys.platform == "darwin" and len(str(path).encode()) >= 100:
        raise ValueError("The socket path is too long for macOS; select a shorter XDG_RUNTIME_DIR")
    return path


def systemd_argument(value):
    """Quote one ExecStart argument and escape systemd's specifiers and variables."""
    value = str(value)
    if any(ord(char) < 32 or ord(char) == 127 for char in value):
        raise ServiceError("A service path contains control characters")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$") + '"'


def ensure_python(required, script):
    """Re-exec a script with a newer interpreter when the host's python3 is too old."""
    if sys.version_info >= required:
        return
    found = find_python(required)
    if found and Path(found).resolve() != Path(sys.executable).resolve():
        os.execv(found, [found, str(script), *sys.argv[1:]])
    wanted = ".".join(str(part) for part in required)
    raise SystemExit(f"codr setup needs Python {wanted} or newer; install one, for example: brew install python@3.12")


def python_version(executable):
    try:
        result = subprocess.run([str(executable), "-c", "import sys; print('%d.%d' % sys.version_info[:2])"],
                                capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode:
        return None
    try:
        return tuple(int(part) for part in result.stdout.strip().split("."))
    except ValueError:
        return None


def find_python(minimum=(3, 10), preferred=("python3.12", "python3.11", "python3.13", "python3.10"), maximum=None):
    """Find a suitable interpreter, preferring Homebrew versions over a too-new global python3."""
    candidates = []
    explicit = os.environ.get("CODR_PYTHON")
    if explicit:
        candidates.append(explicit)
    for name in preferred:
        candidates.append(name)
        for directory in ("/opt/homebrew/bin", "/usr/local/bin"):
            candidates.append(str(Path(directory) / name))
    candidates.extend([sys.executable, "python3"])
    candidates.append("/usr/bin/python3")
    seen = set()
    for candidate in candidates:
        found = candidate if os.sep in str(candidate) else shutil.which(candidate)
        if not found or found in seen:
            continue
        seen.add(found)
        version = python_version(found)
        if version and version >= minimum and (maximum is None or version <= maximum):
            return str(Path(found).resolve())
    return None


def _capture(argv):
    try:
        result = subprocess.run([str(arg) for arg in argv], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout if result.returncode == 0 else None


class SystemdServices:
    backend = SYSTEMD

    def __init__(self, runner, private_write, config_root, error=ServiceError):
        self.runner = runner
        self.private_write = private_write
        self.unit_root = Path(config_root) / "systemd/user"
        self.error = error

    def path(self, service):
        return self.unit_root / f"{service.name}.service"

    def require_tools(self):
        self.runner(["systemctl", "--version"])
        self.runner(["systemctl", "--user", "show-environment"])

    def install(self, service, template=None):
        if template is None:
            template = (ROOT / "systemd" / f"{service.name}.service").read_text()
        try:
            # The unit templates already carry PATH; environment overrides are for launchd.
            text = _replace_directive(template, "ExecStart=",
                                      "ExecStart=" + " ".join(systemd_argument(arg) for arg in service.argv))
        except ServiceError as error:
            raise self.error(str(error)) from error
        target = self.path(service)
        if target.exists() and target.read_text() != text:
            raise self.error(f"Existing service differs from the proposed configuration: {service.name}; nothing was overwritten")
        if not target.exists():
            self.private_write(target, text)
        return target

    def reload(self):
        self.runner(["systemctl", "--user", "daemon-reload"])

    def start(self, names):
        for name in _names(names):
            self.runner(["systemctl", "--user", "start", f"{name}.service"])

    def restart(self, name):
        self.runner(["systemctl", "--user", "restart", f"{name}.service"])

    def is_active(self, names):
        self.runner(["systemctl", "--user", "is-active", *[f"{name}.service" for name in _names(names)]])


class LaunchdServices:
    backend = LAUNCHD

    def __init__(self, runner, private_write, config_root, error=ServiceError):
        self.runner = runner
        self.private_write = private_write
        self.agents = Path(os.environ.get("CODR_LAUNCH_AGENTS_DIR", Path.home() / "Library/LaunchAgents"))
        self.logs = Path.home() / "Library/Logs/codr"
        self.error = error
        self._domain = None

    def path(self, service):
        return self.agents / f"{_label(service)}.plist"

    def domain(self):
        if self._domain:
            return self._domain
        uid = os.getuid()
        for candidate in (f"gui/{uid}", f"user/{uid}"):
            # launchctl print exits 0 for a missing job; only stdout proves it exists.
            if _capture(["launchctl", "print", candidate]):
                self._domain = candidate
                return candidate
        raise self.error("No launchd session domain is available; run setup from a logged-in user session")

    def require_tools(self):
        self.domain()

    def payload(self, service):
        environment = {"PATH": worker_path()}
        environment.update(service.environment)
        return {
            "Label": service.label,
            "ProgramArguments": service.argv,
            "EnvironmentVariables": environment,
            "RunAtLoad": True,
            "KeepAlive": {"SuccessfulExit": False},
            "ThrottleInterval": service.throttle,
            "Umask": 0o077,
            "StandardOutPath": str(self.logs / f"{service.name}.log"),
            "StandardErrorPath": str(self.logs / f"{service.name}.err.log"),
        }

    def install(self, service):
        target = self.path(service)
        text = plistlib.dumps(self.payload(service), fmt=plistlib.FMT_XML, sort_keys=True).decode()
        if target.exists() and target.read_text() != text:
            raise self.error(f"Existing service differs from the proposed configuration: {service.name}; nothing was overwritten")
        self.agents.mkdir(mode=0o755, parents=True, exist_ok=True)
        self.logs.mkdir(mode=0o700, parents=True, exist_ok=True)
        if not target.exists():
            self.private_write(target, text)
        return target

    def loaded(self, service):
        return bool(_capture(["launchctl", "print", f"{self.domain()}/{_label(service)}"]))

    def reload(self):
        """launchd has no daemon-reload; bootstrap reads the plist directly."""

    def ensure_loaded(self, name):
        if not self.loaded(name):
            self.bootstrap(name)

    def bootstrap(self, name):
        try:
            self.runner(["launchctl", "bootstrap", self.domain(), str(self.path(name))])
        except Exception:
            # Preserve PR #16: launchd can refuse bootstrap just after bootout.
            time.sleep(2)
            if not self.loaded(name):
                self.runner(["launchctl", "bootstrap", self.domain(), str(self.path(name))])

    def start(self, names):
        for name in _names(names):
            # RunAtLoad starts a freshly bootstrapped agent; kickstart during that
            # handover aborts the spawn.
            if self.loaded(name):
                self.runner(["launchctl", "kickstart", f"{self.domain()}/{_label(name)}"])
            else:
                self.bootstrap(name)

    def restart(self, name):
        self.ensure_loaded(name)
        self.runner(["launchctl", "kickstart", "-k", f"{self.domain()}/{_label(name)}"])

    def is_active(self, names):
        pending = {_label(name) for name in _names(names)}
        started = time.monotonic()
        while pending:
            spawning = False
            for label in list(pending):
                output = _capture(["launchctl", "print", f"{self.domain()}/{label}"])
                if output and re.search(r"(?m)^\s*state = running\s*$", output):
                    pending.remove(label)
                elif output and re.search(r"(?m)^\s*state = xpcproxy\s*$", output):
                    spawning = True
            if not pending:
                return
            # A first launch can wait in xpcproxy while macOS registers the job.
            if time.monotonic() >= started + (90 if spawning else 15):
                raise self.error(f"Service did not stay running: {', '.join(sorted(pending))}")
            time.sleep(0.25)


def _replace_directive(text, prefix, replacement):
    lines = text.splitlines(keepends=True)
    for index, line in enumerate(lines):
        if line.startswith(prefix):
            ending = "\n" if line.endswith("\n") else ""
            lines[index] = replacement + ending
            return "".join(lines)
    raise ServiceError(f"Service template lacks {prefix.rstrip('=')}")


def _names(names):
    return [names] if isinstance(names, str) else list(names)


def _label(value):
    if isinstance(value, Service):
        return value.label
    name = str(value)
    return name if name.startswith(LABEL_PREFIX + ".") else f"{LABEL_PREFIX}.{name}"
