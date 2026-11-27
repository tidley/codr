#!/usr/bin/env python3
"""Install the project-only adapter and emit a dedicated Mac launch specification.

No services are started, no OpenCode reload/restart is performed, and no grants
are added. The receiving coding agent performs the separately chosen launch and
normal-Enter verification after explicit owner invocation choices.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import stat
import sys
import tempfile


class SetupError(Exception):
    pass


def private_json(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd) as source:
        metadata = os.fstat(source.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                or metadata.st_nlink != 1 or metadata.st_mode & 0o077 or metadata.st_size > 4 * 1024 * 1024):
            raise SetupError("Expected private owner-controlled setup file")
        return json.load(source)


def new_file(path, content):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as target:
        target.write(content)
        target.flush()
        os.fsync(target.fileno())
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def configure(root, package, mode, system=None):
    if (system or sys.platform) != "darwin":
        raise SetupError("This launch helper supports macOS only; use the platform-specific agent guide")
    if root.resolve() != root or package.resolve() != package:
        raise SetupError("Physical absolute setup and package paths required")
    metadata = root.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
        raise SetupError("Setup root must be private and owner-controlled")
    journal = private_json(root / "setup.json")
    binding = private_json(root / "room-input.json")
    if (journal["phase"] != "bound" or journal["options"]["root"] != str(root)
            or journal["key"] != binding["key"] or journal["account"] != binding["account"]
            or journal["native_session"] != binding["sessionID"]
            or journal["options"]["target_group"] != binding["group"]):
        raise SetupError("Completed exact room binding required")
    binary = package / "herdr-white-noise-bridge"
    adapter = package / "adapters/opencode-room-input"
    # Recheck packaged adapter against the reviewed source evidence, including
    # transitive local imports and package metadata. Never invoke npm/bun install.
    evidence = json.loads((package / "CODR-SOURCE.json").read_text())
    import hashlib
    for name in ("index.mjs", "admission.mjs", "transport.mjs", "package.json"):
        relative = "adapters/opencode-room-input/" + name
        if hashlib.sha256((adapter / name).read_bytes()).hexdigest() != evidence["files"].get(relative):
            raise SetupError("Packaged adapter source evidence differs")
    if not binary.is_file():
        raise SetupError("Verified packaged executable required")
    project = Path(journal["options"]["directory"])
    config = project / "opencode.json"
    jsonc = project / "opencode.jsonc"
    if jsonc.exists() or jsonc.is_symlink() or config.is_symlink():
        raise SetupError("Existing JSONC/linked project configuration requires a separate preserving edit")
    original = None
    if config.exists():
        metadata = config.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid() or metadata.st_nlink != 1:
            raise SetupError("Project config must be an owner-controlled regular file")
        # Reject special files before reading, not after a FIFO/device can block.
        original = config.read_bytes()
    settings = json.loads(original) if original is not None else {}
    if not isinstance(settings, dict) or not isinstance(settings.get("plugins", []), list):
        raise SetupError("Unrecognised OpenCode project configuration; nothing replaced")
    plugins = settings.setdefault("plugins", [])
    if any("opencode-room-input" in str(plugin) or "codr.room-input" in str(plugin) for plugin in plugins):
        raise SetupError("Existing room input plugin must not be replaced or duplicated")
    plugins.append({"package": str(adapter), "options": {"binding_file": str(root / "room-input.json")}})
    # One-shot intent persists before either project configuration or service
    # effects. A partially successful invocation cannot be blindly retried.
    new_file(root / "adapter-install-intent.json", json.dumps({"project": str(project), "mode": mode,
        "package": str(package), "key": binding["key"]}).encode())
    if original is not None:
        new_file(root / "opencode-config.before.json", original)
    proposed = json.dumps(settings, indent=2).encode() + b"\n"
    with tempfile.NamedTemporaryFile(dir=project, delete=False) as target:
        temporary = Path(target.name)
        try:
            target.write(proposed)
            target.flush()
            os.fsync(target.fileno())
            if original is not None:
                os.chmod(temporary, stat.S_IMODE(metadata.st_mode))
            # Do not clobber settings changed since the read.
            if (config.read_bytes() if config.exists() else None) != original:
                raise SetupError("Project config changed; preserve intent and inspect")
            if original is None:
                os.link(temporary, config)  # exclusive creation, no replacement
            else:
                os.replace(temporary, config)
        finally:
            temporary.unlink(missing_ok=True)
    # The completion marker lives under root, possibly on a different filesystem.
    # Persist the project's rename before recording successful adapter installation.
    directory = os.open(project, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    argv = [str(binary), "--config", str(root / "config.toml"), "run", "--portr", "--messaging-home", str(root / "messaging")]
    result = {"mode": mode, "argv": argv, "service_started": False,
              "next": "Project hotload only. No OpenCode restart or global location reload. Obtain explicit invocation choices, then verify normal Enter in the exact bound session."}
    if mode == "launchd":
        label = "chat.codr.room." + binding["account"]
        payload = {"Label": label, "ProgramArguments": argv, "WorkingDirectory": str(project),
                   "RunAtLoad": True, "KeepAlive": False,
                   "StandardOutPath": str(root / "service.stdout.log"),
                   "StandardErrorPath": str(root / "service.stderr.log")}
        # Precreate private logs; launchd must not create world-readable logs.
        new_file(root / "service.stdout.log", b"")
        new_file(root / "service.stderr.log", b"")
        unit = root / "room.plist"
        new_file(unit, plistlib.dumps(payload))
        result.update(label=label, plist=str(unit), launch_argv=["launchctl", "bootstrap", f"gui/{os.geteuid()}", str(unit)])
    new_file(root / "adapter-installed.json", json.dumps(result, indent=2).encode())
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--mode", choices=("foreground", "launchd"), required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(configure(args.root, args.package, args.mode), indent=2))
    except (SetupError, OSError, ValueError, KeyError) as error:
        print(f"codr room setup: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
