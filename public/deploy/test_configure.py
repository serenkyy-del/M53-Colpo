import importlib.util
import pathlib
import tempfile
import unittest
spec = importlib.util.spec_from_file_location('configure', pathlib.Path(__file__).with_name('configure.py'))
module = importlib.util.module_from_spec(spec)
class ConfigureTests(unittest.TestCase):
    def test_private_secrets_and_no_overwrite(self):
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp) / 'private'
            module.configure(root)
            files = list(root.iterdir())
            self.assertEqual(len(files), 5)
            self.assertEqual(root.stat().st_mode & 0o777, 0o700)
            original = {p.name: p.read_bytes() for p in files}
            for p in files:
                self.assertEqual(p.stat().st_mode & 0o777, 0o600)
                self.assertGreaterEqual(len(p.read_bytes()), 48)
            with self.assertRaises(FileExistsError): module.configure(root)
            self.assertEqual(original, {p.name: p.read_bytes() for p in files})
if __name__ == '__main__': unittest.main()
