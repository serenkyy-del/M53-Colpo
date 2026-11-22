import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('collect_notices.py')


class NoticeCollectionTest(unittest.TestCase):
    def test_declared_mit_includes_full_terms_and_preserves_published_authors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root/'synthetic'
            crate.mkdir()
            (crate/'Cargo.toml').write_text('[package]\nname = "synthetic"\nversion = "0.1.0"\nlicense = "MIT"\n')
            metadata = root/'metadata.json'
            metadata.write_text(json.dumps({'packages': [{
                'name': 'synthetic', 'version': '0.1.0',
                'source': 'registry+https://github.com/rust-lang/crates.io-index',
                'manifest_path': str(crate/'Cargo.toml'), 'license': 'MIT',
                'license_file': None, 'repository': 'https://example.org/synthetic',
                'authors': ['Published Contributor'],
            }]}))
            output = root/'notice'
            subprocess.run([sys.executable, str(SCRIPT), str(metadata), str(output)], check=True)
            component = output/'synthetic-0.1.0'
            self.assertIn('Permission is hereby granted', (component/'LICENSE-MIT.txt').read_text())
            self.assertIn('Published Contributor', (component/'NOTICE-PACKAGE.txt').read_text())


if __name__ == '__main__':
    unittest.main()
