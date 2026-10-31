"""Confirmed executable updates from pinned ngit records and Blossom assets."""
from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from urllib.parse import urlsplit

import install
import services

AUTHOR = "2f5759825226f1d57ef1652ba66114b2f938f7f5c50dc505708e5d8b31e4f3c9"
APP = f"32267:{AUTHOR}:codr"
REPO = "nostr://npub19at4nqjjymca2lh3v546vcg5ktun3al4c5xu2pts3ewckv0y70ys0gupzr/relay.ngit.dev/codr"
WORKER = "herdr-white-noise-bridge"


class UpdateError(Exception):
    pass


class NotStarted(UpdateError):
    """No independent job was submitted; repair and a fresh check are safe."""


def numeric_version(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value):
        raise UpdateError("The release version is not a stable numeric version")
    return tuple(map(int, value.split(".")))


def run(argv, timeout=60, **kwargs):
    return subprocess.run([str(a) for a in argv], capture_output=True, text=True,
                          check=True, timeout=timeout, **kwargs).stdout


def ngit(arguments):
    executable = shutil.which("ngit")
    if not executable:
        raise UpdateError("Install ngit v3 locally before using /update")
    # ngit v3 currently requires a non-bare Git context, even with an explicit
    # repository URL. This empty private context contains no source or remotes.
    with tempfile.TemporaryDirectory(prefix="codr-ngit-") as context:
        run(["git", "init", "--quiet", context])
        result = json.loads(run([executable, "--repo", REPO, *arguments, "--json"], cwd=context, timeout=90))
    if result.get("format_version") != 2 or result.get("command_status") != "ok":
        raise UpdateError("ngit could not verify the release records")
    return result["result"]


def trusted(release):
    return (release.get("author") == AUTHOR and release.get("application_coordinate") == APP
            and release.get("application_identifier") == "codr" and release.get("channel") == "main"
            and release.get("validation") == [])


def blob_url(url):
    parsed = urlsplit(url)
    host = (parsed.hostname or "").lower().rstrip(".")
    if (parsed.scheme != "https" or not host or parsed.username or parsed.password
            or any(host == suffix or host.endswith("." + suffix)
                   for suffix in ("github.com", "githubusercontent.com", "github.io"))):
        raise UpdateError("Update assets must use independent HTTPS Blossom downloads")
    return url


class BlobRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        blob_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def validate_view(view, expected=None):
    release = view["release"]
    if not trusted(release) or view.get("unresolved_asset_ids") != []:
        raise UpdateError("Release is not a complete stable release from the pinned maintainer")
    version = release["version"]
    numeric_version(version)
    if not re.fullmatch(r"[0-9a-f]{64}", release["event_id"]) or not re.fullmatch(r"[0-9a-f]{40}", release["commit"]):
        raise UpdateError("Release provenance is incomplete")
    name = f"codr-v{version}-{install.platform_name()}.tar.gz"
    chosen = []
    for filename in (name, name + ".sha256"):
        matches = [a for a in view["assets"] if a.get("filename") == filename]
        if len(matches) != 1:
            raise UpdateError("Release lacks this platform's worker archive or checksum")
        a = matches[0]
        if (a.get("author") != AUTHOR or a.get("application_coordinate") != APP
                or a.get("version") != version or a.get("validation") != []
                or a.get("resolution") != "resolved"
                or (filename == name and install.platform_name() not in a.get("platforms", []))
                or not re.fullmatch(r"[0-9a-f]{64}", a.get("sha256", ""))
                or not 0 < int(a.get("size", 0)) <= install.MAX_DOWNLOAD):
            raise UpdateError("The release asset metadata is not valid")
        chosen.append({k: a[k] for k in ("filename", "url", "sha256", "size")})
        blob_url(a["url"])
    offer = {"version": version, "event_id": release["event_id"], "commit": release["commit"],
             "asset": chosen[0], "checksum": chosen[1]}
    if expected is not None and offer != expected:
        raise UpdateError("The release differs from the confirmed offer; check again")
    return offer


def discover(current):
    current = numeric_version(current)
    candidates = []
    for release in ngit(["release", "list"])["releases"]:
        if trusted(release):
            try:
                version = numeric_version(release["version"])
            except UpdateError:
                continue
            candidates.append((version, release))
    if not candidates or max(v for v, _ in candidates) <= current:
        return None
    newest = max(v for v, _ in candidates)
    matches = [r for v, r in candidates if v == newest]
    if len(matches) != 1:
        raise UpdateError("Conflicting stable release records; no update was selected")
    view = ngit(["release", "view", matches[0]["event_id"]])
    offer = validate_view(view)
    if offer["event_id"] != matches[0]["event_id"] or numeric_version(offer["version"]) != newest:
        raise UpdateError("Release identity changed during discovery")
    return offer


def download_blob(release, url, destination):
    metadata = next(a for a in (release["asset"], release["checksum"]) if a["url"] == url)
    opener = urllib.request.build_opener(BlobRedirect())
    with opener.open(urllib.request.Request(blob_url(url), headers={"User-Agent": "codr-updater"}), timeout=60) as response, destination.open("wb") as output:
        blob_url(response.url)
        size = 0
        while chunk := response.read(1024 * 1024):
            size += len(chunk)
            if size > int(metadata["size"]) or size > install.MAX_DOWNLOAD:
                raise UpdateError("The download exceeds its signed size")
            output.write(chunk)
    if size != int(metadata["size"]) or install.sha256(destination) != metadata["sha256"]:
        raise UpdateError("The download does not match its signed hash and size")
    if metadata == release["checksum"]:
        fields = destination.read_text().split()
        if len(fields) != 2 or Path(fields[1].lstrip("*")).name != release["asset"]["filename"]:
            raise UpdateError("The checksum does not name the confirmed archive")


def probe(binary):
    return json.loads(run([binary, "update-info", "--full"], timeout=15))


def check_state(binary, config):
    run([binary, "--config", config, "status"], timeout=15)


def atomic_json(path, value):
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as output:
        json.dump(value, output)
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def atomic_copy(source, destination):
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            with source.open("rb") as stream:
                shutil.copyfileobj(stream, output)
            output.flush()
            os.fsync(output.fileno())
            temporary.chmod(0o755)
            os.replace(temporary, destination)
        finally:
            temporary.unlink(missing_ok=True)


class Controller:
    def __init__(self):
        root = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        self.manager = services.for_host(run, None, root, UpdateError)
        self.service = WORKER
        self.combined = False

    def pid(self):
        if self.manager.backend == services.SYSTEMD:
            return int(run(["systemctl", "--user", "show", self.service + ".service", "--property=MainPID", "--value"]).strip())
        output = run(["launchctl", "print", f"{self.manager.domain()}/{services.LABEL_PREFIX}.{self.service}"])
        found = re.search(r"(?m)^\s*pid = (\d+)\s*$", output)
        return int(found[1]) if found else 0

    def validate(self, request):
        home = request.get("messaging_home")
        selected = request.get("service", WORKER)
        if selected not in ((WORKER, "codr") if home is not None else (WORKER,)):
            raise UpdateError("The update service layout does not match this runtime; use local setup")
        if home is not None and (not isinstance(home, str) or not Path(home).is_absolute() or not Path(home).is_dir()):
            raise UpdateError("The combined messaging home is unavailable; use local setup")
        self.service = selected
        self.combined = home is not None
        destination = Path.home() / ".local/bin" / WORKER
        if Path(request["installed_path"]) != destination or destination.is_symlink():
            raise UpdateError("Automatic updates require the standard managed worker installation")
        if install.sha256(destination) != request["base_sha256"]:
            raise UpdateError("The installed binary changed; check for updates again")
        pid = self.pid()
        if pid <= 0 or pid != request["pid"]:
            raise UpdateError("The managed service is not the bridge that confirmed this update")
        if self.manager.backend == services.SYSTEMD:
            if Path(f"/proc/{pid}/exe").resolve() != destination.resolve():
                raise UpdateError("The managed service is not running this bridge")
            args = Path(f"/proc/{pid}/cmdline").read_bytes().decode().strip("\0").split("\0")
        else:
            import plistlib
            args = plistlib.loads(self.manager.path(self.service).read_bytes())["ProgramArguments"]
        if args.count("--config") > 1 or args.count("run") != 1 or args.count("--messaging-home") != (1 if home is not None else 0):
            raise UpdateError("The managed service arguments do not match this update")
        if home is not None:
            index = args.index("--messaging-home")
            if index + 1 >= len(args) or Path(args[index + 1]).resolve() != Path(home).resolve():
                raise UpdateError("The managed service messaging home does not match this update")
        config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / WORKER / "config.toml"
        if "--config" in args:
            config = Path(args[args.index("--config") + 1])
        if Path(args[0]) != destination or "run" not in args or config.resolve() != Path(request["config_path"]).resolve():
            raise UpdateError("The managed service configuration does not match this chat bridge")

    def stop(self):
        if self.manager.backend == services.SYSTEMD:
            run(["systemctl", "--user", "stop", self.service + ".service"], timeout=150)
        else:
            run(["launchctl", "bootout", f"{self.manager.domain()}/{services.LABEL_PREFIX}.{self.service}"])

    def start(self):
        self.manager.start(self.service)

    def wait_ready(self, directory, version, job_id):
        # A combined first spawn includes launchd registration and MDK storage
        # startup. Never accept a bridge-only or wrong-job receipt as ready.
        deadline = time.monotonic() + (120 if self.combined or self.service == "codr" else 30)
        while time.monotonic() < deadline:
            try:
                ready = json.loads((directory / "ready.json").read_text())
                if ready == {"version": version, "job_id": job_id, "pid": self.pid()} and ready["pid"] > 0:
                    self.manager.is_active(self.service)
                    return True
            except (OSError, ValueError, subprocess.SubprocessError, services.ServiceError, UpdateError):
                pass
            time.sleep(0.5)
        return False


def compatible_info(request, candidate):
    """A combined executable may change implementation, never storage/protocol."""
    expected = {**request["info"], "version": request["release"]["version"]}
    if request.get("messaging_home") is not None:
        current = request["info"]
        cohort = current.get("storage_cohort")
        if (not isinstance(cohort, str) or not cohort or candidate.get("storage_cohort") != cohort
                or "embedded-mdk" not in candidate.get("runtimes", [])):
            raise UpdateError("This release changes the messaging storage cohort; use local setup")
        connector = candidate.get("connector", {})
        previous = current.get("connector", {})
        if (not isinstance(connector, dict) or not isinstance(previous, dict)
                or numeric_version(connector.get("version"))[:2] != numeric_version(previous.get("version"))[:2]
                or not re.fullmatch(r"[0-9a-f]{40}", connector.get("revision", ""))
                or not re.fullmatch(r"[0-9a-f]{64}", connector.get("patch_sha256", ""))):
            raise UpdateError("This release has incompatible messaging provenance; use local setup")
        expected["connector"] = connector
    if candidate != expected:
        raise UpdateError("This release changes the runtime, connector or storage cohort; use local setup")


def perform(directory, request, controller):
    if request.get("action") == "handover":
        import handover
        return handover.perform(directory, request, controller)
    release = request["release"]
    version = release["version"]
    result = {"version": version, "job_id": request["job_id"], "status": "failed",
              "message": "Update failed before installation; the current executable was kept."}
    stopped = replaced = False
    destination = Path(request["installed_path"])
    try:
        if numeric_version(version) <= numeric_version(request["info"]["version"]):
            raise UpdateError("The confirmed release is not newer than the installed bridge")
        controller.validate(request)
        validate_view(ngit(["release", "view", release["event_id"]]), release)
        candidate = directory / "candidate"
        install.install_bundle(release["asset"]["url"], WORKER, candidate,
                               checksum_url=release["checksum"]["url"], expected_sha256=release["asset"]["sha256"],
                               downloader=lambda url, path: download_blob(release, url, path))
        info = probe(candidate)
        compatible_info(request, info)
        check_state(candidate, request["config_path"])
        controller.validate(request)
        stopped = True
        controller.stop()
        shutil.copy2(request["state_path"], directory / "state-backup.json")
        (directory / "state-backup.json").chmod(0o600)
        atomic_copy(destination, directory / "previous")
        atomic_copy(candidate, destination)
        replaced = True
        controller.start()
        if not controller.wait_ready(directory, version, request["job_id"]):
            raise UpdateError("The new bridge did not confirm startup")
        result.update(status="installed", message=f"Updated codr to {version}. Accounts, chats and accepted work were kept.")
    except Exception:
        if stopped:
            try:
                if replaced:
                    controller.stop()
                    atomic_copy(directory / "previous", destination)
                controller.start()
                if not controller.wait_ready(directory, request["info"]["version"], request["job_id"]):
                    raise UpdateError("Rollback did not confirm startup")
                result.update(status="rolled_back", message="Update failed; the previous bridge is running again. Chat state was not rewound.")
            except Exception:
                result["message"] = "Update failed and rollback needs local attention. No automatic retry will run."
    return result


def load_request(directory):
    stat = directory.lstat()
    if directory.is_symlink() or stat.st_uid != os.getuid() or stat.st_mode & 0o077:
        raise UpdateError("The private update job directory is not safe")
    request = json.loads((directory / "request.json").read_text())
    if request["job_id"] != directory.name:
        raise UpdateError("Update job identity does not match")
    return request


def launch(directory):
    try:
        request = load_request(directory)
        Controller().validate(request)
        if request.get("action") == "handover":
            import handover
            handover.Plan(request, Controller())
        python = services.find_python((3, 11))
        if not python:
            raise UpdateError("A supported Python interpreter is unavailable")
    except Exception as error:
        raise NotStarted("The managed updater could not start. Check the local service and Python installation; nothing was installed.") from error
    command = [python, str(directory / "update.py"), "install", str(directory)]
    label = "codr-update-" + directory.name
    if services.detect_backend() == services.SYSTEMD:
        environment = ["--setenv=PATH=" + services.worker_path(), "--setenv=HOME=" + str(Path.home())]
        for name in ("XDG_CONFIG_HOME", "XDG_RUNTIME_DIR"):
            if os.environ.get(name):
                environment.append("--setenv=" + name + "=" + os.environ[name])
        run(["systemd-run", "--user", "--quiet", "--collect", "--property=UMask=0077",
             "--unit=" + label, *environment, "--", *command], timeout=20)
    else:
        run(["launchctl", "submit", "-l", services.LABEL_PREFIX + "." + label,
             "-o", directory / "worker.log", "-e", directory / "worker.log", "--", *command], timeout=20)


def main():
    services.ensure_python((3, 11), __file__)
    os.umask(0o077)
    os.environ["PATH"] = services.worker_path()
    try:
        action = sys.argv[1]
        if action == "check":
            print(json.dumps({"release": discover(sys.argv[2])}))
        elif action == "launch":
            launch(Path(sys.argv[2]))
            print(json.dumps({"started": True}))
        elif action == "install":
            directory = Path(sys.argv[2])
            request = load_request(directory)
            destination = Path(request["installed_path"])
            descriptor = os.open(destination.parent / ".codr-update.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            with os.fdopen(descriptor, "w") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                result = perform(directory, request, Controller())
                atomic_json(directory / "result.json", result)
        else:
            raise UpdateError("Unknown update action")
        return 0
    except Exception as error:
        # Never expose ngit signer diagnostics, credentials or subprocess output.
        message = str(error) if isinstance(error, UpdateError) else "Update operation failed; no automatic retry will run"
        print(json.dumps({"error": message, "not_started": isinstance(error, NotStarted)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
