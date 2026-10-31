"""Run the frozen old updater against a real test executable, without services."""
import hashlib
import os
from pathlib import Path
import shutil
import sys
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]/"scripts"))
import updater_090 as legacy

root = Path(sys.argv[1])
binary = Path(sys.argv[2])
metadata = legacy.probe(binary)
old = {"version":"0.9.0","state_version":1,"protocol":"marmot.agent-control.v2",
       "connector":{"version":"0.14.0","revision":"3fb4b4759ac676b0c21c4c3527074866c9fa4c89",
                    "patch_sha256":"259885539f01cc3a4fd9e49d0f99b20afd8906ea88632d824d16bab354881302"}}
installed = root/"installed"
installed.write_bytes(b"previous 0.9 worker")
job = root/"legacy-job"
job.mkdir(mode=0o700)
request = {"job_id":"legacy-job","info":old,"installed_path":str(installed),
           "config_path":str(root/"config.toml"),"state_path":str(root/"state.json"),
           "release":{"version":metadata["version"],"event_id":"ab"*32,
                      "asset":{"url":"https://blossom.example/archive","sha256":"cd"*32},
                      "checksum":{"url":"https://blossom.example/checksum"}}}
calls = []
controller = SimpleNamespace(validate=lambda request:calls.append("validate"),
                             stop=lambda:calls.append("stop bridge"),
                             start=lambda:calls.append("start bridge"),
                             wait_ready=lambda *args:True)
def stage(url, name, destination, **kwargs):
    shutil.copyfile(binary,destination)
    destination.chmod(0o700)
with patch.object(legacy,"ngit",return_value={}),patch.object(legacy,"validate_view"),patch.object(legacy.install,"install_bundle",side_effect=stage):
    result=legacy.perform(job,request,controller)
assert result["status"]=="installed",result
assert calls==["validate","validate","stop bridge","start bridge"]
with binary.open('rb') as a,installed.open('rb') as b:
    assert hashlib.file_digest(a,'sha256').digest()==hashlib.file_digest(b,'sha256').digest()
assert (job/"previous").read_bytes()==b"previous 0.9 worker"
