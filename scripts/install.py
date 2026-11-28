#!/usr/bin/env python3
"""Install the binary matching this plugin checkout's release version."""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

try:
    import tomllib
except ImportError:  # macOS ships Python 3.9 as /usr/bin/python3
    tomllib = None

ROOT = Path(__file__).resolve().parents[1]
MAX_DOWNLOAD = 256 * 1024 * 1024


class InstallError(Exception):
    pass


def platform_name(system=None, machine=None):
    system = system or platform.system()
    machine = machine or platform.machine()
    architectures = {"x86_64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}
    if machine not in architectures:
        raise InstallError("Supported architectures: x86_64 and aarch64")
    if system == "Linux":
        return "linux-" + architectures[machine]
    if system == "Darwin":
        return "macos-" + architectures[machine]
    raise InstallError("Supported platforms: Linux and macOS on x86_64 and aarch64")


def mdk_platform_name(system=None, machine=None):
    """MDK names its macOS assets with the Darwin system name."""
    return platform_name(system, machine).replace("macos-", "darwin-")


def version():
    text = (ROOT / "Cargo.toml").read_text()
    if tomllib:
        value = tomllib.loads(text)["package"]["version"]
    else:
        # The build hook can run under macOS's Python 3.9, which lacks tomllib.
        found = re.search(r'(?ms)^\[package\]\s*$.*?^version\s*=\s*"([^"]+)"', text)
        if not found:
            raise InstallError("The checkout does not declare a package version")
        value = found.group(1)
    if not valid_version(value):
        raise InstallError("The checkout does not select a valid release")
    return value


def valid_version(value):
    core = r"(?:0|[1-9][0-9]*)"
    identifier = r"(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
    return isinstance(value, str) and re.fullmatch(
        rf"{core}\.{core}\.{core}(?:-{identifier}(?:\.{identifier})*)?", value) is not None


TARGETS = {
    "linux-x86_64": "x86_64-unknown-linux-musl",
    "linux-aarch64": "aarch64-unknown-linux-musl",
    "macos-x86_64": "x86_64-apple-darwin",
    "macos-aarch64": "aarch64-apple-darwin",
}


def binary_platform(path):
    """Check object headers before executing downloaded release bytes."""
    with Path(path).open("rb") as source:
        header = source.read(64)
    if len(header) < 64:
        raise InstallError("Preview executable header is missing")
    if header[:6] == b"\x7fELF\x02\x01":
        kind, cpu = struct.unpack_from("<HH", header, 16)
        if kind in (2, 3) and cpu in (62, 183):
            return "linux-" + ("x86_64" if cpu == 62 else "aarch64")
    if header[:4] == b"\xcf\xfa\xed\xfe":
        cpu = struct.unpack_from("<I", header, 4)[0]
        kind = struct.unpack_from("<I", header, 12)[0]
        if kind == 2 and cpu in (0x01000007, 0x0100000c):
            return "macos-" + ("x86_64" if cpu == 0x01000007 else "aarch64")
    raise InstallError("Preview payload is not a supported native executable")


def probe_binary(path):
    try:
        result = subprocess.run([str(path), "update-info", "--full"], capture_output=True, timeout=15, check=True)
        if len(result.stdout) > 64 * 1024:
            raise InstallError("Preview executable identity exceeds limit")
        return json.loads(result.stdout)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        raise InstallError("Preview executable identity could not be verified") from error


def executable_identity(path, revision):
    host = platform_name()
    if binary_platform(path) != host:
        raise InstallError("Preview executable platform differs from this host")
    verify_signature(path)
    info = probe_binary(path)
    if (not isinstance(info, dict) or info.get("version") != version()
            or info.get("source_commit") != revision or info.get("source_dirty") is not False
            or info.get("target") != TARGETS[host]):
        raise InstallError("Preview executable version, target or source identity differs")
    return {"sha256": sha256(path), "version": info["version"],
            "source_commit": revision, "target": info["target"], "platform": host}


def install_preview(url, destination, downloader=None):
    """Install an immutable opt-in package, checking every shipped source file.

    Never replaces the stable executable, account data or an existing package.
    The checkout supplies the expected source hashes; the downloaded archive's
    checksum alone is not proof that its adapter matches the reviewed checkout.
    """
    if destination.exists() or destination.is_symlink():
        raise InstallError("Preview package already exists; preserve it and inspect before reinstalling")
    fetch = downloader or download
    with tempfile.TemporaryDirectory(prefix="codr-preview-") as temporary:
        archive, checksum = Path(temporary) / "package.tar.gz", Path(temporary) / "checksum"
        fetch(url, archive)
        fetch(url + ".sha256", checksum)
        fields = checksum.read_text().split()
        if not fields or not re.fullmatch(r"[a-fA-F0-9]{64}", fields[0]) or sha256(archive) != fields[0].lower():
            raise InstallError("Preview checksum mismatch")
        from package_evidence import source_files
        expected = {name: sha256(ROOT / name) for name in source_files()}
        # Compatibility copies at the archive root must be the same exact
        # license/notice bytes as the evidence-covered vendored originals.
        licenses = {name: expected["vendor/mdk/" + name]
                    for name in ("LICENSE", "LICENSE-MPL-2.0", "NOTICE.codr", "README.codr.md")}
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".codr-stage-", dir=destination.parent) as stage:
            stage = Path(stage)
            with tarfile.open(archive, "r:gz") as bundle:
                entries = bundle.getmembers()
                names = [entry.name for entry in entries]
                required = set(expected) | {"herdr-white-noise-bridge", "CODR-SOURCE.json", "LICENSE", "LICENSE-MPL-2.0", "NOTICE.codr", "README.codr.md"}
                if len(names) != len(set(names)) or set(names) != required or any(not entry.isfile() or not 0 < entry.size <= MAX_DOWNLOAD for entry in entries):
                    raise InstallError("Unexpected preview package contents")
                evidence = json.load(bundle.extractfile("CODR-SOURCE.json"))
                revision = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
                if not isinstance(evidence, dict) or evidence.get("version") != version() or evidence.get("source_commit") != revision or evidence.get("files") != expected:
                    raise InstallError("Preview source evidence differs from checkout")
                for entry in entries:
                    output = stage / entry.name
                    output.parent.mkdir(parents=True, exist_ok=True)
                    with bundle.extractfile(entry) as source, output.open("wb") as target:
                        shutil.copyfileobj(source, target)
                    digest = expected.get(entry.name, licenses.get(entry.name))
                    if digest is not None and sha256(output) != digest:
                        raise InstallError("Preview source file checksum mismatch")
            binary = stage / "herdr-white-noise-bridge"
            expected_binary = evidence.get("executable")
            if (not isinstance(expected_binary, dict)
                    or expected_binary.get("sha256") != sha256(binary)
                    or expected_binary.get("platform") != platform_name()
                    or binary_platform(binary) != platform_name()):
                raise InstallError("Preview executable checksum or platform differs")
            binary.chmod(0o755)
            if executable_identity(binary, revision) != expected_binary:
                raise InstallError("Preview executable identity differs from release evidence")
            # rename of the complete staged directory is the only activation;
            # a crash before it leaves no usable half-installed package.
            if destination.exists() or destination.is_symlink():
                raise InstallError("Preview destination appeared during installation")
            os.rename(stage, destination)
        return destination / "herdr-white-noise-bridge"


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def verify_signature(path):
    """arm64 macOS refuses to execute unsigned code; fail before replacing a working binary."""
    if platform.system() != "Darwin" or platform.machine() not in ("arm64", "aarch64"):
        return
    with Path(path).open("rb") as source:
        magic = source.read(4)
    if magic not in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"):
        return  # scripts and other non-Mach-O files do not need a signature
    try:
        result = subprocess.run(["codesign", "--verify", "--strict", str(path)], capture_output=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as error:
        raise InstallError("Could not verify the downloaded binary's code signature") from error
    if result.returncode:
        raise InstallError("Downloaded binary has an invalid code signature")


def download(url, destination):
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "codr-installer"})
        with urllib.request.urlopen(request, timeout=60) as response, destination.open("wb") as output:
            if not response.url.startswith("https://"):
                raise InstallError("Release download redirected outside HTTPS")
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > MAX_DOWNLOAD:
                    raise InstallError("Release download exceeds 256 MiB")
                output.write(chunk)
    except (OSError, ValueError) as error:
        raise InstallError(f"Could not download release asset: {url}") from error


def install_bundle(url, member, destination, *, checksum_url=None, expected_sha256=None, downloader=None):
    """Verify the archive, then install only its expected regular executable."""
    with tempfile.TemporaryDirectory(prefix="codr-download-") as temporary:
        archive = Path(temporary) / "binary.tar.gz"
        checksum = Path(temporary) / "checksum"
        fetch = downloader or download
        fetch(url, archive)
        fetch(checksum_url or url + ".sha256", checksum)
        fields = checksum.read_text().split()
        if not fields or not re.fullmatch(r"[a-fA-F0-9]{64}", fields[0]):
            raise InstallError("Invalid release checksum")
        if sha256(archive) != fields[0].lower():
            raise InstallError("Release checksum mismatch; nothing was installed")
        if expected_sha256 is not None and sha256(archive) != expected_sha256:
            raise InstallError("Release differs from the confirmed asset; nothing was installed")
        with tarfile.open(archive, "r:gz") as bundle:
            entries = [entry for entry in bundle.getmembers() if entry.name == member]
            if len(entries) != 1 or not entries[0].isfile() or not 0 < entries[0].size <= MAX_DOWNLOAD:
                raise InstallError("Release lacks the expected regular executable")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
                temporary_binary = Path(output.name)
                try:
                    with bundle.extractfile(entries[0]) as source:
                        shutil.copyfileobj(source, output)
                    output.flush()
                    os.fsync(output.fileno())
                    os.chmod(temporary_binary, 0o755)
                    verify_signature(temporary_binary)
                    os.replace(temporary_binary, destination)
                finally:
                    temporary_binary.unlink(missing_ok=True)


def connector_path(release):
    return Path.home() / f".local/share/codr/bin/wn-agent-codr-v{release}"


def install_connector(release):
    asset = f"codr-wn-agent-v{release}-{platform_name()}.tar.gz"
    destination = connector_path(release)
    install_bundle(f"https://github.com/tidley/codr/releases/download/v{release}/{asset}", "wn-agent", destination)
    return destination


def main():
    try:
        release = version()
        asset = f"codr-v{release}-{platform_name()}.tar.gz"
        url = f"https://github.com/tidley/codr/releases/download/v{release}/{asset}"
        if "-" in release:
            destination = Path.home() / f".local/share/codr/packages/v{release}"
            binary = install_preview(url, destination)
            print(f"Installed opt-in codr {release}: {binary}")
            print("Stable command and services unchanged. Follow docs/AGENT-INSTALL.md; do not invoke legacy Setup/Start.")
            return 0
        destination = Path.home() / ".local/bin/herdr-white-noise-bridge"
        combined = tuple(map(int, release.split("."))) >= (0, 10, 0)
        connector = None if combined else install_connector(release)
        install_bundle(url, "herdr-white-noise-bridge", destination)
        print(f"Installed codr {release}: {destination}")
        if connector:
            print(f"Dedicated connector: {connector}")
            print("Next: run the plugin's Setup action, or follow docs/INSTALL.md for non-interactive setup.")
            print("For an existing installation, run Setup to select the matching connector and preserve account state.")
        else:
            print("One executable contains the bridge and MDK messaging runtime.")
            print("Existing accounts require the explicit stopped-backup combined-service handover; installation does not migrate services.")
            print("Next: follow docs/COMBINED.md for the approved local handover, not the legacy Setup action.")
            print("Fresh accounts require 0.9 setup before handover. Do not start the combined runtime on unapproved storage.")
    except (InstallError, OSError, ValueError, subprocess.SubprocessError, tarfile.TarError) as error:
        print(f"codr install: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
