#!/usr/bin/env python3
"""Collect immutable crates.io source archives for both published Cargo locks."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from pathlib import Path
import shutil
import tomllib
import urllib.request
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
LOCKS = [ROOT/'Cargo.lock', ROOT/'prototypes/gpui-editor/Cargo.lock']


def inventory():
    result = {}
    for lock in LOCKS:
        for package in tomllib.loads(lock.read_text())['package']:
            if 'source' not in package:
                continue
            if package['source'] != 'registry+https://github.com/rust-lang/crates.io-index':
                raise ValueError(f"Unhandled source for {package['name']}")
            key = (package['name'], package['version'])
            if key in result and result[key] != package['checksum']:
                raise ValueError(f'Conflicting crate checksum: {key}')
            result[key] = package['checksum']
    return result


def sha256(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def locate(entry, download, caches):
    (name, version), checksum = entry
    filename = f'{name}-{version}.crate'
    for cache in caches:
        candidate = next((p for p in cache.glob(f'*/{filename}') if p.is_file()), None)
        if candidate and sha256(candidate) == checksum:
            return candidate
    target = download/filename
    url = f'https://static.crates.io/crates/{name}/{filename}'
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=40) as media, target.open('wb') as destination:
                shutil.copyfileobj(media, destination)
            if sha256(target) != checksum:
                raise ValueError(f'Checksum mismatch: {filename}')
            return target
        except OSError:
            if attempt == 2:
                raise
            time.sleep(1 + attempt)
    raise AssertionError('unreachable')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--cache', action='append', type=Path, default=[])
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True,exist_ok=True)
    caches = [*args.cache, Path.home()/'.cargo/registry/cache']
    packages = sorted(inventory().items())
    with tempfile.TemporaryDirectory() as scratch, ThreadPoolExecutor(max_workers=12) as workers:
        obtained = workers.map(lambda package: locate(package, Path(scratch), caches), packages)
        with tarfile.open(args.output, 'x:gz', compresslevel=3) as archive:
            for path in obtained:
                archive.add(path, arcname=f'crates/{path.name}', recursive=False)
    print(f'Verified {len(packages)} locked crate sources')


if __name__ == '__main__':
    main()
