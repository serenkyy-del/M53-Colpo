#!/usr/bin/env python3
"""Audit the artifacts together before publishing a public release."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib
import zipfile

SOURCE = 'Colposcopy-Source.zip'
DEPENDENCIES = 'Colposcopy-DependencySources.tar.gz'
WINDOWS = 'Colposcopy-Windows-x64.zip'
DEBIAN = 'Colposcopy-Linux-x64.deb'
SERVER = 'Colposcopy-Server-Linux-x64.tar.gz'
PACKAGE_PATH = re.compile(r'[A-Za-z0-9._+/-]+')
PRIVATE_PATHS = {'.git', '.dev', 'secrets', 'private', 'CA'}
PRIVATE_CONTENT = re.compile(
    rb'm-53\.ru|192\.168\.73\.(?:22|30)|10\.111\.0\.22|(?:secrets|\.dev)[/](?:project|server-install)'
    rb'|colpo[\s_-]*zoom|-----BEGIN [A-Z ]*PRIVATE KEY-----|github_pat_[A-Za-z0-9_]{30,}',
    re.I,
)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def hash_stream(handle):
    return hashlib.file_digest(handle, 'sha256').hexdigest()


def package_path(name):
    return bool(PACKAGE_PATH.fullmatch(name)) and all(p not in ('', '.', '..') for p in name.split('/'))


def zip_contents(archive):
    files = [i.filename for i in archive.infolist() if not i.is_dir()]
    require(len(set(name.casefold() for name in files)) == len(files), 'Archive has duplicate file paths')
    for name in files:
        require(package_path(name), f'Unsafe archive path: {name}')
    return set(files)


def audit_source(path):
    locked = {}
    with zipfile.ZipFile(path) as archive:
        names = zip_contents(archive)
        require({'LICENSE', 'README.md', '.github/workflows/release.yml'} <= names, 'Incomplete source archive')
        require(b'GNU AFFERO GENERAL PUBLIC LICENSE' in archive.read('LICENSE'), 'Source archive license changed')
        for name in names:
            require(not (set(PurePosixPath(name).parts) & PRIVATE_PATHS), f'Private path in source: {name}')
            require(not PRIVATE_CONTENT.search(archive.read(name)), f'Private content in source: {name}')
        for name in ('Cargo.lock', 'prototypes/gpui-editor/Cargo.lock'):
            for package in tomllib.loads(archive.read(name).decode('utf-8'))['package']:
                if 'source' not in package:
                    continue
                require(package['source'] == 'registry+https://github.com/rust-lang/crates.io-index',
                        f"Unsupported source: {package['name']}")
                crate = f"crates/{package['name']}-{package['version']}.crate"
                digest = package['checksum']
                require(crate not in locked or locked[crate] == digest, f'Conflicting checksums: {crate}')
                locked[crate] = digest
        return locked, archive.read('assets/print-runtime/manifest.json')


def audit_dependencies(path, locked):
    pending = locked.copy()
    with tarfile.open(path, 'r|gz') as archive:
        for member in archive:
            require(member.isfile() and member.name in pending, f'Unexpected dependency source: {member.name}')
            with archive.extractfile(member) as handle:
                require(hash_stream(handle) == pending.pop(member.name), f'Invalid source hash: {member.name}')
    require(not pending, f'Missing dependency sources: {len(pending)}')


def audit_windows(path, print_manifest, release_url):
    with zipfile.ZipFile(path) as archive:
        names = zip_contents(archive)
        required = {
            'Colposcopy.exe', 'Setup.ps1', 'Install-PrintRuntime.ps1', 'README.txt', 'print-runtime-manifest.json',
            'package-manifest.json', 'licenses/LICENSE', 'licenses/FONT-LICENSE.txt', 'licenses/rust/DEPENDENCIES.tsv',
        }
        require(required <= names, 'Incomplete Windows package')
        require(sum(name.endswith('.exe') for name in names) == 1, 'Unexpected Windows executable')
        require(not any(name.endswith(('.zip', '.dll')) for name in names), 'Windows archive bundles external binaries')
        package_pins = json.loads(archive.read('print-runtime-manifest.json'))
        require(package_pins == json.loads(print_manifest), 'Print pins differ from the source')
        manifest = json.loads(archive.read('package-manifest.json'))
        files = {item['path']: item for item in manifest['files']}
        require(manifest['format'] == 1 and len(files) == len(manifest['files']) and
                names == files.keys() | {'package-manifest.json'}, 'Windows manifest does not match its archive')
        for name, item in files.items():
            require(archive.getinfo(name).file_size == item['size'], f'Size mismatch: {name}')
            with archive.open(name) as handle:
                require(hash_stream(handle) == item['sha256'], f'Hash mismatch: {name}')
        program = archive.read('Colposcopy.exe')
        require(program.startswith(b'MZ') and not PRIVATE_CONTENT.search(program), 'Invalid or private Windows binary')
        require(release_url.encode() in program, 'Windows source link does not match the release')
        require(b'GNU AFFERO GENERAL PUBLIC LICENSE' in archive.read('licenses/LICENSE'), 'Windows license changed')


def audit_debian(path, release_url):
    metadata = subprocess.check_output(['dpkg-deb', '-f', str(path), 'Architecture'], text=True)
    require(metadata.strip() == 'amd64', 'Unexpected Debian architecture')
    with subprocess.Popen(['dpkg-deb', '--fsys-tarfile', str(path)], stdout=subprocess.PIPE) as process:
        found = {}
        with tarfile.open(fileobj=process.stdout, mode='r|') as archive:
            for f in archive:
                if f.name in ('./usr/bin/colposcopy', './usr/share/applications/colposcopy.desktop',
                             './usr/share/doc/colposcopy-desktop/copyright'):
                    with archive.extractfile(f) as handle:
                        found[f.name] = handle.read()
        require(process.wait() == 0, 'Could not read Debian package contents')
    program = found['./usr/bin/colposcopy']
    require(program.startswith(b'\x7fELF') and not PRIVATE_CONTENT.search(program), 'Invalid or private Linux binary')
    require(release_url.encode() in program, 'Linux source link does not match the release')
    require(b'Exec=colposcopy' in found['./usr/share/applications/colposcopy.desktop'], 'Missing launcher')
    require(b'GNU AFFERO GENERAL PUBLIC LICENSE' in found['./usr/share/doc/colposcopy-desktop/copyright'],
            'Debian package license changed')


def audit_server(path):
    with tarfile.open(path) as archive:
        require(not any(member.name.endswith(('.tar', '.zip')) for member in archive), 'Server redistributes an OS image')
        with archive.extractfile('./public/deploy/bin/colposcopy-server') as handle:
            program = handle.read()
        require(program.startswith(b'\x7fELF') and not PRIVATE_CONTENT.search(program), 'Invalid or private server binary')
        with archive.extractfile('./public/deploy/compose.yaml') as handle:
            compose = handle.read()
        require(b'image: debian:bookworm-slim' in compose and b'command: ["serve"]' in compose, 'Invalid server runtime')
        require(b'    build:' not in compose and not PRIVATE_CONTENT.search(compose), 'Server compose requires building or private resources')
        require('./licenses/DEPENDENCIES.tsv' in archive.getnames(), 'Missing server dependency notices')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--release-url', required=True)
    args = parser.parse_args()
    root = args.directory
    locked, runtime = audit_source(root/SOURCE)
    audit_dependencies(root/DEPENDENCIES, locked)
    audit_windows(root/WINDOWS, runtime, args.release_url)
    audit_debian(root/DEBIAN, args.release_url)
    audit_server(root/SERVER)
    print(f'PASS source snapshot, {len(locked)} pinned crate sources and all installable packages')


if __name__ == '__main__':
    main()
