#!/usr/bin/env python3
"""Build the release-specific license companion from the exact public archives."""
import argparse
from contextlib import closing
import csv
import hashlib
import io
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

from licensing import SUPPLEMENTS, TEXTS, load_catalog, load_supplements, reference_files
from verify_release import DEBIAN, DEPENDENCIES, SERVER, SOURCE, WINDOWS, audit_source


def crate_inventory(path, locked, catalog):
    pending = locked.copy()
    result = {}
    with tarfile.open(path, 'r|gz') as archive:
        for item in archive:
            if not item.isfile() or item.name not in pending:
                raise ValueError(f'Unexpected dependency source: {item.name}')
            with archive.extractfile(item) as handle:
                raw = handle.read()
            if hashlib.sha256(raw).hexdigest() != pending.pop(item.name):
                raise ValueError(f'Source hash differs from Cargo.lock: {item.name}')
            with tarfile.open(fileobj=io.BytesIO(raw), mode='r:gz') as crate:
                manifest_name = f'{Path(item.name).stem}/Cargo.toml'
                with crate.extractfile(manifest_name) as handle:
                    metadata = tomllib.loads(handle.read().decode('utf-8'))['package']
                name, version = metadata['name'], metadata['version']
                if item.name != f'crates/{name}-{version}.crate' or (name, version) in result:
                    raise ValueError(f'Source identity mismatch: {item.name}')
                expression = metadata.get('license')
                if not expression:
                    raise ValueError(f'Unknown license: {item.name}')
                refs = reference_files(expression, catalog)
                result[name, version] = (
                    expression, item.name, metadata.get('repository') or '',
                    ', '.join(f'Reference-Licenses/{p.name}' for p in refs),
                )
    if pending:
        raise ValueError(f'Missing dependency archives: {len(pending)}')
    return result


def read_platform_inventories(root):
    with zipfile.ZipFile(root/WINDOWS) as archive:
        windows = archive.read('licenses/rust/DEPENDENCIES.tsv')
    with tarfile.open(root/SERVER, 'r:gz') as archive:
        with archive.extractfile('./licenses/DEPENDENCIES.tsv') as handle:
            server = handle.read()
    linux = None
    with subprocess.Popen(['dpkg-deb', '--fsys-tarfile', str(root/DEBIAN)], stdout=subprocess.PIPE) as child:
        with tarfile.open(fileobj=child.stdout, mode='r|') as archive:
            for item in archive:
                if item.name.endswith('/rust/DEPENDENCIES.tsv'):
                    with archive.extractfile(item) as handle:
                        linux = handle.read()
        if child.wait() or linux is None:
            raise ValueError('Could not read the Linux dependency notices')
    return {'Windows': windows, 'Linux': linux, 'Server': server}


def write_platform_inventory(path, inventory, sources, supplements, catalog):
    reader = csv.DictReader(io.StringIO(inventory.decode('utf-8-sig')), delimiter='\t')
    with path.open('w', encoding='utf-8', newline='') as stream:
        writer = csv.writer(stream, delimiter='\t', lineterminator='\n')
        writer.writerow((*reader.fieldnames, 'complete license references', 'source', 'supplemental notices'))
        count = 0
        for row in reader:
            component = (row['name'], row['version'])
            if component == ('gpui-pre-windows', '0.3.9'):
                source = 'Colposcopy-Source.zip:prototypes/gpui-editor/vendor/gpui-pre-windows'
            else:
                declared, location, *_ = sources[component]
                if declared != row['declared license']:
                    raise ValueError(f'Crate license declaration differs from published source: {component}')
                source = f'Colposcopy-DependencySources.tar.gz:{location}'
            refs = reference_files(row['declared license'], catalog)
            restored = [f'Supplemental-Notices/{p.parent.name}/{p.name}' for p in supplements.get(component, [])]
            writer.writerow([row[field] for field in reader.fieldnames] + [
                ', '.join(f'Reference-Licenses/{p.name}' for p in refs), source, ', '.join(restored),
            ])
            count += 1
    return count


def write_source_inventory(path, sources):
    with path.open('w', encoding='utf-8', newline='') as stream:
        writer = csv.writer(stream, delimiter='\t', lineterminator='\n')
        writer.writerow(('name', 'version', 'declared license', 'archive path', 'upstream', 'complete license references'))
        for (name, version), data in sorted(sources.items()):
            writer.writerow((name, version, *data))


def package(root, output, release_url):
    catalog = load_catalog()
    supplements = load_supplements()
    locked, _ = audit_source(root/SOURCE)
    sources = crate_inventory(root/DEPENDENCIES, locked, catalog)
    inventories = read_platform_inventories(root)
    with tempfile.TemporaryDirectory() as temporary:
        stage = Path(temporary)
        shutil.copytree(TEXTS, stage/'Reference-Licenses')
        shutil.copytree(SUPPLEMENTS, stage/'Supplemental-Notices')
        with zipfile.ZipFile(root/SOURCE) as archive:
            for name, original in (
                ('LICENSE', 'LICENSE'),
                ('FONT-LICENSE.txt', 'crates/client/assets/FONT-LICENSE.txt'),
                ('GPUI-Windows/LICENSE-APACHE', 'prototypes/gpui-editor/vendor/gpui-pre-windows/LICENSE-APACHE'),
                ('GPUI-Windows/PATCHES.md', 'prototypes/gpui-editor/vendor/gpui-pre-windows/PATCHES.md'),
            ):
                target = stage/name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.read(original))
            patched = stage/'GPUI-Windows/src'
            patched.mkdir()
            for name in ('directx_devices.rs', 'directx_renderer.rs'):
                original = f'prototypes/gpui-editor/vendor/gpui-pre-windows/src/{name}'
                code = archive.read(original)
                header = b'// Modified by M53-Colpo. The local changes are described in ../PATCHES.md.\n'
                if not code.startswith(header):
                    code = header + code
                (patched/name).write_bytes(code)

        write_source_inventory(stage/'Cargo-Source-Inventory.tsv', sources)
        counts = {name: write_platform_inventory(stage/f'{name}-Inventory.tsv', inventory, sources, supplements, catalog)
                  for name,inventory in inventories.items()}
        prose = [
            '# M53-Colpo third-party license notices', '',
            f'Release: {release_url}', '',
            'Keep this archive with the installation packages and their matching source archives.', '',
            'The application code is AGPL-3.0-only. Dependency files retain their published licenses.',
            'See the platform inventories for the package-specific notices and corresponding source locations.',
            'The original crate archives preserve upstream copyright and authorship information.',
            'GPUI-Windows/src supplies the two changed GPUI source files with their Apache change notices.',
            'Reference-Licenses supplies standard license texts. Supplemental-Notices identifies precise upstream',
            'attributions and texts for releases that did not include a separate notice file.', '',
            'Where the declared expression offers a choice, the release uses Apache-2.0 for self_cell',
            'and for r-efi in the source archive. The original license expressions and texts are preserved.', '',
            'DejaVu Sans is redistributed with its Bitstream Vera terms. The GPL stanza in the Debian',
            'notice covers Debian packaging files. The archive ships the font and its notice.', '',
            'Windows setup retrieves verified Chromium Headless and PDF.js archives directly from their',
            'upstream download locations. The server pulls the official Debian and PostgreSQL images.',
            'The Linux package uses system libraries. These upstream runtimes are installed separately.', '',
            f'Cargo source archives verified: {len(sources)}.',
            *[f'{name} package inventory entries: {count}.' for name,count in counts.items()], '',
        ]
        (stage/'README.md').write_text('\n'.join(prose), encoding='utf-8')
        with zipfile.ZipFile(output, 'x', zipfile.ZIP_DEFLATED) as bundle:
            for f in sorted(stage.rglob('*')):
                if f.is_file():
                    bundle.write(f, f.relative_to(stage).as_posix())
    print(f'PASS {len(sources)} sources and platform inventories {counts}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--release-url', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    package(args.directory, args.output, args.release_url)


if __name__ == '__main__':
    main()
