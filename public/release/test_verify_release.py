import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

from verify_release import audit_windows


class WindowsArchiveTest(unittest.TestCase):
    def test_accepts_real_versioned_notice_path_and_windows_manifest_line_endings(self):
        release_url = 'https://example.org/releases/preview'
        print_manifest = b'{\n  "format": 1,\n  "sources": []\n}\n'
        files = {
            'Colposcopy.exe': b'MZ' + release_url.encode(),
            'Setup.ps1': b'# Setup',
            'Install-PrintRuntime.ps1': b'# Print setup',
            'README.txt': b'Preview',
            'print-runtime-manifest.json': print_manifest.replace(b'\n', b'\r\n'),
            'licenses/LICENSE': b'GNU AFFERO GENERAL PUBLIC LICENSE',
            'licenses/FONT-LICENSE.txt': b'Font license',
            'licenses/rust/DEPENDENCIES.tsv': b'TOML package',
            'licenses/rust/toml-1.1.8+spec-1.1.0/LICENSE-MIT': b'TOML license',
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'preview.zip'
            with zipfile.ZipFile(path, 'w') as archive:
                for name, content in files.items():
                    archive.writestr(name, content)
                archive.writestr('package-manifest.json', json.dumps({
                    'format': 1,
                    'files': [
                        {'path': name, 'size': len(content), 'sha256': hashlib.sha256(content).hexdigest()}
                        for name, content in files.items()
                    ],
                }))
            audit_windows(path, print_manifest, release_url)


if __name__ == '__main__':
    unittest.main()
