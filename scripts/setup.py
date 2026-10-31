#!/usr/bin/env python3
"""Configure a dedicated codr account and user services without replacing existing state."""

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
import plistlib
import shlex
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))

if __name__ == "__main__":
    import services as _services
    # macOS still ships Python 3.9 as /usr/bin/python3 and tomllib needs 3.11.
    _services.ensure_python((3, 11), __file__)

import tomllib

import services
from install import InstallError, connector_path, install_connector, platform_name, version, sha256
from phonon import configure_phonon

ROOT = Path(__file__).resolve().parents[1]
MDK_VERSION = tomllib.loads((ROOT / "connector.toml").read_text())["version"]


class SetupError(Exception):
    pass


def run(argv):
    environment = dict(os.environ)
    if sys.platform != "darwin":
        environment.setdefault("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
        environment.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={environment['XDG_RUNTIME_DIR']}/bus")
    try:
        result = subprocess.run([str(arg) for arg in argv], capture_output=True, text=True, timeout=180, env=environment)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SetupError(f"Could not complete {Path(argv[0]).name}; inspect its local service log") from error
    if result.returncode:
        raise SetupError(f"{Path(argv[0]).name} failed (exit {result.returncode}); inspect its local service log")
    return result.stdout


def find_executable(name, candidates=()):
    # Keep the PATH location instead of the symlink target. Homebrew upgrades
    # replace the versioned Cellar path, which would break a stored executable.
    found = shutil.which(name)
    if found:
        return os.path.abspath(found)
    candidates = [Path(candidate) for candidate in candidates]
    if sys.platform == "darwin":
        candidates.extend([Path("/opt/homebrew/bin") / name, Path("/usr/local/bin") / name])
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate.absolute())
    raise SetupError(f"Required executable not found: {name}")


def control_request(path, payload):
    payload = dict(payload, id=str(uuid.uuid4()))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
        control.settimeout(30)
        control.connect(str(path))
        control.sendall((json.dumps(payload) + "\n").encode())
        with control.makefile("rb") as incoming:
            raw = incoming.readline(1024 * 1024 + 1)
        if not raw.endswith(b"\n") or len(raw) > 1024 * 1024:
            raise SetupError("Invalid local socket response")
        response = json.loads(raw)
    if response.get("id") != payload["id"] or "error" in response or response.get("type") == "error":
        raise SetupError("Local socket request was not confirmed")
    return response


def read_snapshot(path):
    response = control_request(path, {"method": "session.snapshot", "params": {}})
    snapshot = response.get("result", {}).get("snapshot")
    if not isinstance(snapshot, dict) or not isinstance(snapshot.get("agents"), list):
        raise SetupError("herdr did not return a session snapshot")
    return snapshot


def local_accounts(path):
    deadline = time.monotonic() + 20
    while True:
        try:
            response = control_request(path, {"marmot_agent_control": "marmot.agent-control.v2", "type": "account_list"})
            if response.get("marmot_agent_control") != "marmot.agent-control.v2" or response.get("type") != "account_list":
                raise SetupError("MDK account selection was not confirmed")
            return response["accounts"]
        except (OSError, TimeoutError):
            if time.monotonic() >= deadline:
                raise SetupError("MDK socket is unavailable; inspect wn-agent-herdr.service")
            time.sleep(0.25)


def publish_pat_profile(socket_path, result, record_path):
    """Name only a newly created identity, journalling before the public profile write."""
    account = result["account_id_hex"]
    if not isinstance(account, str) or not re.fullmatch(r"[0-9a-f]{64}", account):
        raise SetupError("Invalid bridge account for profile publication")
    previous = json.loads(record_path.read_text()) if record_path.exists() else {}
    if previous and (not isinstance(previous, dict) or previous.get("account_id_hex") != account):
        raise SetupError("Stored bootstrap identity differs from the selected bridge account")
    result = {**previous, **result}
    status = previous.get("default_profile_status")
    if status is not None:
        if status not in ("pending", "unconfirmed", "published", "preserved"):
            raise SetupError("Stored profile publication needs inspection")
        result["default_profile_status"] = "unconfirmed" if status == "pending" else status
        return result
    if result.get("created") is not True:
        result["default_profile_status"] = "preserved"
        return result
    result["default_profile_status"] = "pending"
    result["default_profile_name"] = "Pat"
    private_write(record_path, json.dumps(result, indent=2) + "\n")
    try:
        response = control_request(socket_path, {"marmot_agent_control": "marmot.agent-control.v2",
            "type": "account_publish_profile", "account_id_hex": account, "name": "Pat", "display_name": "Pat"})
        expected = {"marmot_agent_control": "marmot.agent-control.v2", "type": "profile_published",
            "account_id_hex": account, "name": "Pat", "display_name": "Pat"}
        if not isinstance(response, dict) or any(response.get(key) != value for key, value in expected.items()):
            raise SetupError("Pat profile publication was not confirmed")
    except (SetupError, OSError, ValueError):
        result["default_profile_status"] = "unconfirmed"
        return result
    result["default_profile_status"] = "published"
    private_write(record_path, json.dumps(result, indent=2) + "\n")
    return result


def ensure_mdk(runner):
    dedicated = connector_path(version())
    if not dedicated.exists():
        install_connector(version())
    if runner([str(dedicated), "--version"]).strip() != f"wn-agent {MDK_VERSION}":
        raise SetupError("The dedicated codr connector could not be verified on this host")
    return dedicated


def upgrade_transport(existing, config_root, runner):
    binary = ensure_mdk(runner)
    manager = services.for_host(runner, private_write, config_root, error=SetupError)
    if manager.backend == services.LAUNCHD:
        return upgrade_launchd_transport(existing, binary, manager, runner)
    unit = config_root / "systemd/user/wn-agent-herdr.service"
    if not unit.is_file():
        raise SetupError("The existing transport unit is missing; inspect the installation")
    if any(unit.with_name(unit.name + ".d").glob("*.conf")):
        raise SetupError(f"Custom transport overrides need inspection; select the dedicated connector at {binary}")
    if unit.is_symlink():
        raise SetupError("The transport unit is linked to a custom file; inspect it before upgrading")
    dropins = runner(["systemctl", "--user", "show", "wn-agent-herdr.service", "-p", "DropInPaths", "--value"]).strip()
    fragment = runner(["systemctl", "--user", "show", "wn-agent-herdr.service", "-p", "FragmentPath", "--value"]).strip()
    if dropins or (fragment and Path(fragment).resolve() != unit.resolve()):
        raise SetupError(f"Effective transport overrides need inspection; select the dedicated connector at {binary}")
    original = unit.read_text()
    matches = list(re.finditer(r'(?m)^ExecStart=("(?:[^"\\]|\\.)*"|\S+)([^\n]*)$', original))
    if len(matches) != 1:
        raise SetupError("The transport unit has an unexpected command; nothing was replaced")
    command = matches[0]
    if command.group(1) == unit_argument(binary):
        return False
    executable = command.group(1).strip('"')
    if not (Path(executable).name in ("wn-agent", f"wn-agent-{MDK_VERSION}") or re.fullmatch(r"wn-agent-codr-v\d+\.\d+\.\d+", Path(executable).name)):
        raise SetupError("The transport unit is customised; inspect its executable before upgrading")
    old_binary = shlex.split(command.group(1))[0].replace("%%", "\0").replace("%h", str(Path.home()))
    if "%" in old_binary:
        raise SetupError("The transport executable uses custom systemd specifiers; inspect it before upgrading")
    old_binary = old_binary.replace("\0", "%")
    if runner([old_binary, "--version"]).strip() != f"wn-agent {MDK_VERSION}":
        raise SetupError("The existing connector is outside the supported MDK cohort; inspect it before upgrading")
    expected_socket = Path.home() / ".marmot-agents/herdr/dev/wn-agent.sock"
    if Path(existing["wn_agent_socket"]) != expected_socket:
        raise SetupError("The configured transport socket is customised; inspect it before upgrading")
    proposed = original[:command.start(1)] + unit_argument(binary) + original[command.end(1):]
    state_path = Path(existing["state_path"])
    def settled():
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if state.get("media_pending") or any(m.get("creating") for m in state.get("mappings", {}).values()):
            raise SetupError("Wait for media preparation and group creation before upgrading the connector")
    settled()
    backup = unit.with_name(unit.name + f".before-codr-v{version()}")
    if not backup.exists():
        private_write(backup, original)
    try:
        runner(["systemctl", "--user", "stop", "herdr-white-noise-bridge.service"])
        settled()
        runner(["systemctl", "--user", "stop", "wn-agent-herdr.service"])
        private_write(unit, proposed)
        runner(["systemctl", "--user", "daemon-reload"])
        runner(["systemctl", "--user", "start", "wn-agent-herdr.service"])
        runner(["systemctl", "--user", "start", "herdr-white-noise-bridge.service"])
        manager.is_active(["wn-agent-herdr", "herdr-white-noise-bridge"])
    except BaseException as error:
        failures = []
        for action in (lambda: runner(["systemctl", "--user", "stop", "herdr-white-noise-bridge.service"]),
                       lambda: runner(["systemctl", "--user", "stop", "wn-agent-herdr.service"]),
                       lambda: private_write(unit, original),
                       lambda: runner(["systemctl", "--user", "daemon-reload"]),
                       lambda: runner(["systemctl", "--user", "start", "wn-agent-herdr.service"]),
                       lambda: runner(["systemctl", "--user", "start", "herdr-white-noise-bridge.service"])):
            try:
                action()
            except Exception:
                failures.append("restoration failed")
        if not isinstance(error, Exception):
            raise
        raise SetupError("Connector upgrade failed; inspect the service logs" + ("; service restoration also needs inspection" if failures else "")) from error
    return True


def upgrade_launchd_transport(existing, binary, manager, runner):
    unit = manager.path("wn-agent-herdr")
    if not unit.is_file() or unit.is_symlink():
        raise SetupError("The transport agent is missing or linked; inspect it before upgrading")
    original = unit.read_text()
    payload = plistlib.loads(original.encode())
    argv = payload.get("ProgramArguments")
    label = f"{services.LABEL_PREFIX}.wn-agent-herdr"
    if payload.get("Label") != label or "Program" in payload or not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
        raise SetupError("The transport agent has a custom command; nothing was replaced")
    if argv[0] == str(binary):
        return False
    if not (Path(argv[0]).name in ("wn-agent", f"wn-agent-{MDK_VERSION}") or re.fullmatch(r"wn-agent-codr-v\d+\.\d+\.\d+", Path(argv[0]).name)):
        raise SetupError("The transport agent is customised; inspect its executable before upgrading")
    if runner([argv[0], "--version"]).strip() != f"wn-agent {MDK_VERSION}":
        raise SetupError("The existing connector is outside the supported MDK cohort; inspect it before upgrading")
    home = Path.home() / ".marmot-agents/herdr"
    expected_socket = home / "dev/wn-agent.sock"
    if Path(existing["wn_agent_socket"]) != expected_socket or argv.count("--home") != 1 or argv.count("--socket") != 1:
        raise SetupError("The transport home or socket is customised; inspect it before upgrading")
    for flag, value in (("--home", home), ("--socket", expected_socket)):
        index = argv.index(flag) + 1
        if index >= len(argv) or argv[index] != str(value):
            raise SetupError("The transport home or socket is customised; inspect it before upgrading")
    payload["ProgramArguments"] = [str(binary), *argv[1:]]
    proposed = plistlib.dumps(payload, fmt=plistlib.FMT_XML, sort_keys=True).decode()
    state_path = Path(existing["state_path"])
    def settled():
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        if state.get("media_pending") or any(m.get("creating") for m in state.get("mappings", {}).values()):
            raise SetupError("Wait for media preparation and group creation before upgrading the connector")
    def stop(name):
        if manager.loaded(name):
            runner(["launchctl", "bootout", f"{manager.domain()}/{services.LABEL_PREFIX}.{name}"])
    settled()
    backup = unit.with_name(unit.name + f".before-codr-v{version()}")
    if not backup.exists():
        private_write(backup, original)
    try:
        stop("herdr-white-noise-bridge")
        settled()
        stop("wn-agent-herdr")
        private_write(unit, proposed)
        manager.start("wn-agent-herdr")
        manager.start("herdr-white-noise-bridge")
        manager.is_active(["wn-agent-herdr", "herdr-white-noise-bridge"])
    except BaseException as error:
        failures = []
        for action in (lambda: stop("herdr-white-noise-bridge"), lambda: stop("wn-agent-herdr"),
                       lambda: private_write(unit, original), lambda: manager.start("wn-agent-herdr"),
                       lambda: manager.start("herdr-white-noise-bridge")):
            try:
                action()
            except Exception:
                failures.append("restoration failed")
        if not isinstance(error, Exception):
            raise
        raise SetupError("Connector upgrade failed; inspect the service logs" + ("; service restoration also needs inspection" if failures else "")) from error
    return True


def toml_text(values):
    return "".join(f"{key} = {json.dumps(value if isinstance(value, (list, bool, int)) else str(value), ensure_ascii=False)}\n" for key, value in values.items())


def check_readers(bridge, config_path, runner):
    try:
        result = json.loads(runner([str(bridge), "--config", str(config_path), "check-readers"]))
        checked = result["checked_sessions"]
        if type(checked) is not int or checked < 0:
            raise ValueError("Invalid reader check count")
    except (SetupError, ValueError, KeyError, TypeError) as error:
        raise SetupError("Session history check failed; verify CLI export support and native transcript access before starting services") from error
    return checked


def repair_opencode_bin(bridge, existing, config_path, runner, check):
    """Repair a configured OpenCode path that an upgrade or uninstall removed."""
    configured = existing.get("opencode_bin")
    # Bare commands remain PATH-based and OpenCode is optional for native agents.
    # Session reader validation decides whether a live session needs OpenCode.
    if not configured or not Path(configured).is_absolute():
        return False
    if Path(configured).is_file() and os.access(configured, os.X_OK):
        return False
    try:
        replacement = find_executable("opencode", [Path.home() / ".local/bin/opencode", Path.home() / ".opencode/bin/opencode"])
    except SetupError:
        return False
    if check:
        raise SetupError(f"The configured OpenCode executable is missing; rerun setup to select {replacement}")
    if replacement == configured:
        return False
    text = config_path.read_text()
    setting = toml_text({"opencode_bin": replacement}).rstrip()
    text, changed = re.subn(r'(?m)^opencode_bin\s*=\s*"[^"]*"(?:[ \t]*#[^\n]*)?$', lambda _: setting, text, count=1)
    if changed != 1:
        raise SetupError("Could not replace the missing OpenCode executable; inspect the configuration")
    with tempfile.TemporaryDirectory(prefix="codr-opencode-check-") as temporary:
        candidate = Path(temporary) / "config.toml"
        candidate.write_text(text)
        runner([str(bridge), "--config", str(candidate), "status"])
        check_readers(bridge, candidate, runner)
    private_write(config_path, text)
    existing["opencode_bin"] = replacement
    return True


def private_write(path, text):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            os.chmod(temporary, 0o600)
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def unit_argument(value):
    """Kept for callers and tests; systemd quoting now lives in services.py."""
    try:
        return services.systemd_argument(value)
    except services.ServiceError as error:
        raise SetupError(str(error)) from error


@contextmanager
def setup_lock(directory):
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(directory / "setup.lock", os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise SetupError("Another codr setup is already running") from error
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def configure(owner, herdr_socket=None, runner=run, check=False, enable_audio=None, enable_portr=None):
    if check:
        return _configure(owner, herdr_socket, runner, check, enable_audio, enable_portr)
    directory = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "herdr-white-noise-bridge"
    with setup_lock(directory):
        return _configure(owner, herdr_socket, runner, check, enable_audio, enable_portr)


def _configure(owner, herdr_socket, runner, check, enable_audio, enable_portr):
    if check and enable_portr is not None:
        raise SetupError("--check cannot apply --portr or --no-portr")
    platform_name()
    home = Path.home()
    config_root = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")).resolve()
    config_path = config_root / "herdr-white-noise-bridge/config.toml"
    bridge = home / ".local/bin/herdr-white-noise-bridge"
    manager = services.for_host(runner, private_write, config_root, error=SetupError)
    existing = tomllib.loads(config_path.read_text()) if config_path.exists() else None
    owner = owner or (existing["phone_npub"] if existing else None)
    if not owner:
        raise SetupError("Supply your White Noise public npub with --npub; never supply a private key")
    # Use the executable's own Bech32/config validation before services or accounts are changed.
    with tempfile.TemporaryDirectory(prefix="codr-owner-check-") as temporary:
        directory = Path(temporary)
        candidate = directory / "config.toml"
        candidate.write_text(toml_text({"phone_npub": owner, "account_id_hex": "00" * 32,
            "herdr_socket": directory / "herdr.sock", "wn_agent_socket": directory / "wn.sock", "state_path": directory / "state.json"}))
        runner([str(bridge), "--config", str(candidate), "status"])
    if existing:
        if existing["phone_npub"].lower() != owner.lower():
            raise SetupError("Existing configuration belongs to a different phone; nothing was replaced")
        selected = herdr_socket or os.environ.get("HERDR_SOCKET_PATH")
        if selected and Path(selected).resolve() != Path(existing["herdr_socket"]).resolve():
            raise SetupError("Existing configuration targets another herdr server; nothing was replaced")
        runner([str(bridge), "--config", str(config_path), "status"])
        original_config = config_path.read_text()
        repaired_opencode = repair_opencode_bin(bridge, existing, config_path, runner, check)
        if repaired_opencode:
            try:
                manager.restart("herdr-white-noise-bridge")
            except BaseException:
                # Keep the repair detectable on the next setup attempt.
                private_write(config_path, original_config)
                raise
        checked = check_readers(bridge, config_path, runner)
        portr_changed = False
        if not check and enable_portr is not None and existing.get("portr_enabled", False) != enable_portr:
            text = config_path.read_text()
            value = str(enable_portr).lower()
            expected = {**existing, "portr_enabled": enable_portr}
            if "portr_enabled" in existing:
                # Matching text can be inside a multiline setting; choose by parsed meaning.
                pattern = r"(?m)^([ \t]*portr_enabled[ \t]*=[ \t]*)(?:true|false)([ \t]*(?:#[^\n]*)?)$"
                for match in re.finditer(pattern, text):
                    candidate = text[:match.start()] + match[1] + value + match[2] + text[match.end():]
                    try:
                        if tomllib.loads(candidate) == expected:
                            text = candidate
                            break
                    except tomllib.TOMLDecodeError:
                        continue
                else:
                    raise SetupError("Could not change portr_enabled; inspect the configuration")
            else:
                text += ("" if text.endswith("\n") else "\n") + f"portr_enabled = {value}\n"
            if tomllib.loads(text) != expected:
                raise SetupError("Portr setting would change other configuration; nothing was written")
            with tempfile.TemporaryDirectory(prefix="codr-portr-check-") as temporary:
                candidate = Path(temporary) / "config.toml"
                candidate.write_text(text)
                runner([str(bridge), "--config", str(candidate), "status"])
            private_write(config_path, text)
            existing["portr_enabled"] = enable_portr
            portr_changed = True
        if not check:
            upgrade_transport(existing, config_root, runner)
        if not check and enable_audio:
            command = configure_phonon(runner, private_write, unit_argument)
            if existing.get("transcribe_command") and existing["transcribe_command"] != command:
                raise SetupError("An existing transcription command is configured; inspect it before selecting Phonon")
            if existing.get("transcribe_command") != command:
                text = config_path.read_text()
                setting = toml_text({"transcribe_command": command}).rstrip()
                if "transcribe_command" in existing:
                    text, changed = re.subn(r"(?m)^transcribe_command\s*=\s*\[\s*\][ \t]*(?:#[^\n]*)?$", lambda _: setting, text, count=1)
                    if changed != 1:
                        raise SetupError("Could not replace the empty transcription setting; inspect the configuration")
                else:
                    text = text.rstrip() + "\n" + setting + "\n"
                with tempfile.TemporaryDirectory(prefix="codr-audio-check-") as temporary:
                    candidate = Path(temporary) / "config.toml"
                    candidate.write_text(text)
                    runner([str(bridge), "--config", str(candidate), "status"])
                private_write(config_path, text)
                manager.restart("herdr-white-noise-bridge")
        if not check:
            manager.start("herdr-white-noise-bridge")
        report = {"configuration": str(config_path), "existing_configuration_preserved": True,
                  "bridge_account_id": existing["account_id_hex"], "phone_round_trip": "requires user confirmation",
                  "checked_session_readers": checked, "reader_validation": "passed" if checked else "deferred: no live supported sessions"}
        report["portr_enabled"] = existing.get("portr_enabled", False)
        report["portr_restart_required"] = portr_changed
        bootstrap_file = Path(existing["wn_agent_socket"]).parent.parent / "codr-bootstrap.json"
        if bootstrap_file.exists():
            bootstrap = json.loads(bootstrap_file.read_text())
            if bootstrap.get("account_id_hex") != existing["account_id_hex"]:
                raise SetupError("Stored bootstrap identity differs from the configured bridge account")
            report["bridge_npub"] = bootstrap["npub"]
            report["default_profile_status"] = bootstrap.get("default_profile_status", "preserved")
        return report
    if check:
        raise SetupError("No existing codr configuration to check")

    if manager.backend == services.SYSTEMD:
        find_executable("systemctl")
    manager.require_tools()
    find_executable("git")
    herdr_socket = Path(herdr_socket or os.environ.get("HERDR_SOCKET_PATH", home / ".config/herdr/herdr.sock")).resolve()
    snapshot = read_snapshot(herdr_socket)
    executables = {}
    for name in ("opencode", "codex", "claude"):
        candidates = [home / ".local/bin" / name]
        if name == "opencode":
            candidates.append(home / ".opencode/bin/opencode")
        try:
            executable = find_executable(name, candidates)
            runner([executable, "--version"])
        except SetupError:
            continue
        executables[name] = executable
    observed = {agent.get("agent_session", {}).get("agent", agent.get("agent")) for agent in snapshot["agents"] if isinstance(agent.get("agent_session", {}), dict)}
    available = set(executables) | observed.intersection({"codex", "claude"})
    if not available:
        raise SetupError("Install or start a supported agent: OpenCode, Codex, or Claude Code")
    opencode = executables.get("opencode", "opencode")
    with tempfile.TemporaryDirectory(prefix="codr-reader-check-") as temporary:
        directory = Path(temporary)
        candidate = directory / "config.toml"
        candidate.write_text(toml_text({"phone_npub": owner, "account_id_hex": "00" * 32,
            "herdr_socket": herdr_socket, "wn_agent_socket": directory / "wn.sock", "state_path": directory / "state.json",
            "opencode_bin": opencode, "codex_home": Path(os.environ.get("CODEX_HOME", home / ".codex")).resolve(),
            "claude_home": Path(os.environ.get("CLAUDE_CONFIG_DIR", home / ".claude")).resolve()}))
        checked = check_readers(bridge, candidate, runner)
    transcribe_command = configure_phonon(runner, private_write, unit_argument) if enable_audio is not False else []
    mdk = ensure_mdk(runner)
    mdk_home = home / ".marmot-agents/herdr"
    mdk_socket = mdk_home / "dev/wn-agent.sock"
    state_path = Path(os.environ.get("XDG_STATE_HOME", home / ".local/state")) / "herdr-white-noise-bridge/state.json"
    image_spool = state_path.with_suffix(".outbound-media")
    image_spool.mkdir(mode=0o700, parents=True, exist_ok=True)
    if image_spool.is_symlink() or not image_spool.is_dir() or image_spool.stat().st_mode & 0o077:
        raise SetupError("The outbound image spool must be a private directory")
    manager.install(services.Service("wn-agent-herdr", "MDK transport for herdr White Noise remote",
        [mdk, "--home", mdk_home, "--socket", mdk_socket,
         "--relay", "wss://relay.eu.whitenoise.chat", "--relay", "wss://relay.us.whitenoise.chat",
         "--media-allowed-root", image_spool]))
    manager.install(services.Service("herdr-white-noise-bridge", "herdr White Noise remote plugin worker",
        [bridge, "--config", config_path, "run"], environment={"PATH": services.worker_path()}))
    mdk_home.mkdir(mode=0o700, parents=True, exist_ok=True)
    manager.reload()
    manager.start("wn-agent-herdr")
    accounts = local_accounts(mdk_socket)
    candidates = [account for account in accounts if account.get("local_signing") and account.get("label") == "herdr-bridge"]
    if len(candidates) > 1 or (accounts and not candidates):
        raise SetupError("Dedicated MDK home contains ambiguous accounts; select the existing identity manually")
    bootstrap = [str(mdk), "bootstrap", "--home", str(mdk_home), "--socket", str(mdk_socket),
        "--label", "herdr-bridge", "--allow-welcomer", owner, "--no-quic", "--json"]
    if candidates:
        bootstrap.extend(["--no-create", "--account-id-hex", candidates[0]["account_id_hex"]])
    result = json.loads(runner(bootstrap))
    if candidates and result["account_id_hex"] != candidates[0]["account_id_hex"]:
        raise SetupError("MDK selected an unexpected account")
    values = {"phone_npub": owner, "herdr_socket": herdr_socket, "wn_agent_socket": mdk_socket,
        "account_id_hex": result["account_id_hex"], "state_path": state_path, "opencode_bin": opencode,
        "codex_home": Path(os.environ.get("CODEX_HOME", home / ".codex")).resolve(),
        "claude_home": Path(os.environ.get("CLAUDE_CONFIG_DIR", home / ".claude")).resolve(),
        "transcribe_command": transcribe_command}
    # Old release binaries reject unknown fields; phone-only setup needs no desktop setting.
    if enable_portr is True:
        values["portr_enabled"] = True
    with tempfile.TemporaryDirectory(prefix="codr-config-check-") as temporary:
        candidate = Path(temporary) / "config.toml"
        candidate.write_text(toml_text(values))
        runner([str(bridge), "--config", str(candidate), "status"])
    result = publish_pat_profile(mdk_socket, result, mdk_home / "codr-bootstrap.json")
    private_write(mdk_home / "codr-bootstrap.json", json.dumps(result, indent=2) + "\n")
    private_write(config_path, toml_text(values))
    manager.start("herdr-white-noise-bridge")
    manager.is_active(["wn-agent-herdr", "herdr-white-noise-bridge"])
    return {"configuration": str(config_path), "bridge_npub": result["npub"], "herdr_socket": str(herdr_socket),
            "default_profile_status": result["default_profile_status"], "default_profile_name": result.get("default_profile_name"),
            "portr_enabled": values.get("portr_enabled", False), "portr_restart_required": False,
            "detected_sessions": len(snapshot["agents"]), "supported_agents": sorted(available),
            "checked_session_readers": checked, "reader_validation": "passed" if checked else "deferred: no live supported sessions",
            "phone_round_trip": "requires user confirmation"}


def configure_combined(config_path, messaging_home, backup, previous_connector, approved=False, runner=run):
    """Explicit existing-account handover. Never stop services or restore account state."""
    if not approved:
        raise SetupError("Combined handover requires explicit approval after stopping services and backing up account credentials locally")
    paths = [Path(p) for p in (config_path, messaging_home, backup, previous_connector)]
    if not all(p.is_absolute() for p in paths):
        raise SetupError("Combined handover paths must be absolute")
    config_path, home, backup, previous_connector = [p.resolve() for p in paths]
    if not config_path.is_file() or not home.is_dir() or not backup.is_dir() or not previous_connector.is_file():
        raise SetupError("Existing configuration, messaging home, private backup and previous connector are required")
    if backup.is_relative_to(home) or home.is_relative_to(backup):
        raise SetupError("The backup must be separate from the live messaging home")
    metadata = backup.stat()
    if metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
        raise SetupError("The backup must be private and owned by this user")
    with setup_lock(config_path.parent):
        config = tomllib.loads(config_path.read_text())
        if not Path(config["state_path"]).is_absolute() or not Path(config["wn_agent_socket"]).is_absolute():
            raise SetupError("Existing state and messaging socket paths must be absolute for managed handover")
        state_path = Path(config["state_path"]).resolve()
        if json.loads(state_path.read_text()).get("update_job") is not None:
            raise SetupError("Resolve the pending independent update before service handover; do not start a second installer")
        identity = home / "codr-bootstrap.json"
        if not identity.is_file() or json.loads(identity.read_text()).get("account_id_hex") != config["account_id_hex"]:
            raise SetupError("The messaging home's recorded identity does not match the existing configuration")
        root = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        manager = services.for_host(runner, private_write, root, error=SetupError)
        old_names = ("herdr-white-noise-bridge", "wn-agent-herdr")
        if manager.backend == services.SYSTEMD:
            for name in (*old_names, "codr"):
                if int(runner(["systemctl", "--user", "show", name + ".service", "--property=MainPID", "--value"]).strip() or "0"):
                    raise SetupError("Stop both old services and any combined service before handover")
                active = runner(["systemctl", "--user", "show", name + ".service", "--property=ActiveState", "--value"]).strip()
                if active not in ("inactive", "failed", ""):
                    raise SetupError("Wait for services to stop before handover")
            for name in old_names:
                state = runner(["systemctl", "--user", "show", name + ".service", "--property=UnitFileState", "--value"]).strip()
                if state not in ("disabled", "masked", ""):
                    raise SetupError("Disable the old service startup before handover")
            old_paths = [manager.unit_root / (name + ".service") for name in old_names]
        else:
            if any(manager.loaded(name) for name in (*old_names, "codr")):
                raise SetupError("Boot out old and combined launchd jobs before handover")
            disabled = runner(["launchctl", "print-disabled", manager.domain()])
            if any(not re.search(r'"' + re.escape(services.LABEL_PREFIX + "." + name) + r'"\s*=>\s*true', disabled) for name in old_names):
                raise SetupError("Disable old launchd job startup before handover")
            old_paths = [manager.path(name) for name in old_names]
        if runner([str(previous_connector), "--version"]).strip() != f"wn-agent {MDK_VERSION}":
            raise SetupError("The old connector is outside the supported cohort; migration needs separate approval")
        binary = Path.home() / ".local/bin/herdr-white-noise-bridge"
        info = json.loads(runner([str(binary), "update-info", "--full"]))
        if ("embedded-mdk" not in info.get("runtimes", []) or info.get("storage_cohort") != "mdk-" + ".".join(MDK_VERSION.split(".")[:2])
                or info.get("connector", {}).get("version") != MDK_VERSION):
            raise SetupError("The installed executable is not a compatible combined runtime")
        pairs = [(config_path, backup / "config.toml"), (state_path, backup / "state.json")]
        if any(path.is_symlink() for path in backup.rglob("*")):
            raise SetupError("The private backup must not link to live files")
        source_files = set()
        for source in home.rglob("*"):
            if source.is_symlink():
                raise SetupError("Symlinked account storage needs manual backup inspection")
            if source.is_file():
                source_files.add(source.relative_to(home))
                pairs.append((source, backup / "mdk-home" / source.relative_to(home)))
        saved_files = {path.relative_to(backup / "mdk-home") for path in (backup / "mdk-home").rglob("*") if path.is_file()}
        if source_files != saved_files:
            raise SetupError("The account backup file set differs from the stopped home")
        pairs.extend((path, backup / "services" / path.name) for path in old_paths if path.exists())
        if not (backup / "previous").is_file() or (backup / "previous").is_symlink():
            raise SetupError("Keep the previous executable in the private backup")
        if any(not saved.is_file() or saved.is_symlink() or sha256(source) != sha256(saved) for source, saved in pairs):
            raise SetupError("The stopped private backup does not match existing account, state, config or services")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
            control.settimeout(1)
            try:
                control.connect(str(config["wn_agent_socket"]))
            except (FileNotFoundError, ConnectionRefusedError):
                pass
            except OSError as error:
                raise SetupError("Messaging socket ownership needs local inspection before handover") from error
            else:
                raise SetupError("An unmanaged messaging socket owner is still running; nothing was installed")
        # The embedded runtime repeats socket/home ownership checks. A failed
        # start is not permission to recreate an identity or rewind MLS state.
        runner([str(binary), "--config", str(config_path), "status"])
        manager.install(services.combined_service(binary, config_path, home))
        manager.reload()
        if manager.backend == services.SYSTEMD:
            runner(["systemctl", "--user", "enable", "codr.service"])
        try:
            if manager.backend == services.LAUNCHD:
                runner(["launchctl", "enable", manager.domain() + "/" + services.LABEL_PREFIX + ".codr"])
            manager.start("codr")
            manager.is_active("codr")
            accounts = local_accounts(Path(config["wn_agent_socket"]))
            if not any(account.get("account_id_hex") == config["account_id_hex"] and account.get("local_signing") is True for account in accounts):
                raise SetupError("The combined service did not confirm the existing messaging identity")
        except Exception as error:
            try:
                if manager.backend == services.SYSTEMD:
                    runner(["systemctl", "--user", "stop", "codr.service"])
                    runner(["systemctl", "--user", "disable", "codr.service"])
                else:
                    runner(["launchctl", "bootout", manager.domain() + "/" + services.LABEL_PREFIX + ".codr"])
                    runner(["launchctl", "disable", manager.domain() + "/" + services.LABEL_PREFIX + ".codr"])
            except Exception:
                raise SetupError("Combined startup and cleanup need local inspection; account data was not restored") from error
            raise SetupError("Combined startup failed; inspect locally before restarting compatible services. Account data was not restored") from error
        return {"service":"codr", "configuration":str(config_path), "messaging_home":str(home),
                "existing_configuration_preserved":True, "phone_round_trip":"requires user confirmation"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--npub", default=os.environ.get("CODR_OWNER_NPUB") or None)
    parser.add_argument("--herdr-socket", type=Path)
    parser.add_argument("--check", action="store_true", help="Validate an existing configuration without changing services")
    parser.add_argument("--combined-home", type=Path, help="Hand over an existing MDK account home to the single codr service")
    parser.add_argument("--backup", type=Path, help="Private stopped backup containing mdk-home, config.toml, state.json, services and previous executable")
    parser.add_argument("--previous-connector", type=Path, help="Existing connector executable used to verify the storage cohort")
    parser.add_argument("--approve-handover", action="store_true", help="Confirm old services are stopped/disabled and a complete private backup includes secret-store credentials")
    audio = parser.add_mutually_exclusive_group()
    audio.add_argument("--phonon", dest="enable_audio", action="store_true", default=None, help="Install or reuse the local Phonon-2 worker, including on an existing installation")
    audio.add_argument("--no-audio", dest="enable_audio", action="store_false", help="Skip audio setup on a fresh installation")
    desktop = parser.add_mutually_exclusive_group()
    desktop.add_argument("--portr", dest="enable_portr", action="store_true", default=None,
        help="Enable the optional portr desktop/SSH endpoint; does not install SSH or the desktop app")
    desktop.add_argument("--no-portr", dest="enable_portr", action="store_false", default=None,
        help="Disable the optional desktop endpoint (fresh installations default to disabled)")
    args = parser.parse_args()
    config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "herdr-white-noise-bridge/config.toml"
    if not args.npub and not config.exists() and sys.stdin.isatty():
        args.npub = input("Your White Noise public npub: ").strip()
    if not args.check and not config.exists() and args.enable_portr is None and sys.stdin.isatty():
        try:
            args.enable_portr = input("Enable optional portr desktop/SSH access? [y/N]: ").strip().lower() in ("y", "yes")
        except EOFError:
            args.enable_portr = False
    try:
        if args.combined_home is not None:
            if args.check or args.backup is None or args.previous_connector is None or args.enable_portr is not None or args.enable_audio is not None or args.npub or args.herdr_socket:
                raise SetupError("Use --combined-home with --backup, --previous-connector and --approve-handover; do not combine it with other setup changes")
            print(json.dumps(configure_combined(config, args.combined_home, args.backup, args.previous_connector, args.approve_handover), indent=2))
            return 0
        if not args.check and tuple(map(int, version().split("."))) >= (0, 10, 0):
            raise SetupError("0.10 uses one combined service. For an existing account, use the approved --combined-home handover. Fresh identity setup remains available through 0.9 local setup before handover.")
        report = configure(args.npub, args.herdr_socket, check=args.check, enable_audio=args.enable_audio, enable_portr=args.enable_portr)
        print(json.dumps(report, indent=2))
        if report.get("default_profile_status") == "unconfirmed":
            print("Pat's profile publication was not confirmed. The existing identity was kept; inspect its profile before trying another publication. Phone setup can still be used.")
        if report.get("portr_restart_required"):
            print("Portr setting saved. Apply it by restarting only the codr worker after all active work is idle. Setup did not restart the worker for this setting.")
        print("Accept a verified bridge invitation in White Noise, then send /status and a harmless test prompt.")
    except (SetupError, InstallError, OSError, ValueError, KeyError) as error:
        print(f"codr setup: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
