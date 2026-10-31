"""Frozen probe and perform from f0b150de385811021aaaf685c07401c9abf04537.

Shared download/file helpers are imported, but the shipped control flow and
strict metadata equality below must not change to make a compatibility test pass.
"""
import json
from pathlib import Path
import shutil
from update import (UpdateError, WORKER, atomic_copy, check_state, download_blob,
                    install, ngit, numeric_version, run, validate_view)


def probe(binary):
    return json.loads(run([binary, "update-info"], timeout=15))


def perform(directory, request, controller):
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
        if info != {**request["info"], "version": version}:
            raise UpdateError("This release changes the connector or storage cohort; use local setup")
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
