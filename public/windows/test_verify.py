import importlib.util
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import zipfile
spec=importlib.util.spec_from_file_location('verify',Path(__file__).with_name('verify.py'))
class ArchiveTests(unittest.TestCase):
 def test_accept_exact_and_reject_modified_or_extra(self):
  module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
  with tempfile.TemporaryDirectory() as tmp:
   archive=Path(tmp)/'runtime.zip';manifest=Path(tmp)/'manifest.json'
   payload=b'synthetic runtime';record={'path':'chrome/headless.exe','size':len(payload),'sha256':hashlib.sha256(payload).hexdigest()}
   def write(extra=False):
    with zipfile.ZipFile(archive,'w') as z:
     z.writestr(record['path'],payload)
     if extra:z.writestr('../escape',b'x')
    data={'format':1,'archive_size':archive.stat().st_size,'archive_sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),'files':[record]}
    manifest.write_text(json.dumps(data))
   write();module.verify_runtime(archive,manifest)
   archive.write_bytes(archive.read_bytes()+b'x')
   with self.assertRaises(ValueError):module.verify_runtime(archive,manifest)
   write(True)
   with self.assertRaises(ValueError):module.verify_runtime(archive,manifest)
if __name__=='__main__':unittest.main()
