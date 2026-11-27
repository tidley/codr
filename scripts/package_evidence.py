#!/usr/bin/env python3
"""Record exact preview source files; no builds or publication."""
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile


def source_files():
    return [
        "vendor/mdk/LICENSE", "vendor/mdk/README.codr.md",
        "vendor/mdk/LICENSE-MPL-2.0", "vendor/mdk/NOTICE.codr",
        "Cargo.toml", "Cargo.lock", "herdr-plugin.toml",
        "build.rs", "src/main.rs", "src/lib.rs", "src/room_setup.rs",
        "src/native_rooms.rs", "src/bridge/room_access.rs", "src/update.rs",
        "adapters/opencode-room-input/index.mjs",
        "adapters/opencode-room-input/admission.mjs",
        "adapters/opencode-room-input/transport.mjs",
        "adapters/opencode-room-input/package.json",
        "docs/AGENT-INSTALL.md", "scripts/room_setup.py",
        "scripts/install.py", "scripts/package_evidence.py",
    ]


def evidence(root):
    sys.path.insert(0, str(root / "scripts"))
    import install
    return {"version": install.version(), "source_commit": subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True,
        text=True, check=True).stdout.strip(), "files": {
            name: hashlib.sha256((root / name).read_bytes()).hexdigest()
            for name in source_files()}}


def package(root, binary, destination):
    sys.path.insert(0, str(root / "scripts"))
    import install
    # Inspect the binary before opening the output: source files and an archive
    # checksum cannot turn unrelated executable bytes into this release.
    if subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD", "--"]).returncode:
        raise install.InstallError("Release packaging requires a clean committed source checkout")
    identity = install.executable_identity(binary, subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True,
        text=True, check=True).stdout.strip())
    record = evidence(root)
    record["executable"] = identity
    data = json.dumps(record, indent=2).encode() + b"\n"
    with tarfile.open(destination, "w:gz") as bundle:
        bundle.add(binary, arcname="herdr-white-noise-bridge", recursive=False)
        for name in source_files():
            bundle.add(root / name, arcname=name, recursive=False)
        for name in ("LICENSE", "README.codr.md", "LICENSE-MPL-2.0", "NOTICE.codr"):
            bundle.add(root / "vendor/mdk" / name, arcname=name, recursive=False)
        entry = tarfile.TarInfo("CODR-SOURCE.json")
        entry.size = len(data)
        bundle.addfile(entry, io.BytesIO(data))


if __name__ == "__main__":
    package(Path(__file__).resolve().parents[1], sys.argv[1], sys.argv[2])
