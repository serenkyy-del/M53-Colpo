import importlib.util
import hashlib
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('provision',Path(__file__).with_name('provision-runtime.py'))
class ProvisionTests(unittest.TestCase):
 def test_deterministic_and_reject_modified_source(self):
  module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);(root/'sample').write_bytes(b'synthetic')
   files=[{'path':'sample','size':9,'sha256':hashlib.sha256(b'synthetic').hexdigest()}]
   module.pack(root,files,root/'a.zip');module.pack(root,files,root/'b.zip')
   self.assertEqual((root/'a.zip').read_bytes(),(root/'b.zip').read_bytes())
   (root/'sample').write_bytes(b'tampered!')
   with self.assertRaises(ValueError):module.pack(root,files,root/'bad.zip')
if __name__=='__main__':unittest.main()
