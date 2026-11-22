#!/usr/bin/env python3
"""Preserve bundled licenses and published metadata for the resolved crates."""
import argparse
import csv
import json
from pathlib import Path
import shutil
import urllib.request

PREFIXES = ('license', 'licence', 'notice', 'copying', 'copyright', 'unlicense', 'third-party')
UPSTREAM = Path(__file__).with_name('upstream-notices.tsv')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('metadata', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.mkdir(exist_ok=False, parents=True)
    packages = json.loads(args.metadata.read_text(encoding='utf-8-sig'))['packages']
    with UPSTREAM.open(encoding='utf-8') as table:
        extra = list(csv.DictReader(table, delimiter='\t'))
    with (args.output/'DEPENDENCIES.tsv').open('w', encoding='utf-8', newline='') as report:
        inventory = csv.writer(report, delimiter='\t')
        inventory.writerow(('name', 'version', 'declared license', 'upstream', 'files'))
        for p in sorted(packages, key=lambda p: (p['name'], p['version'])):
            if not p['source'] and p['name'] != 'gpui-pre-windows':
                continue
            source = Path(p['manifest_path']).parent
            target = args.output / f"{p['name']}-{p['version']}"
            target.mkdir()
            for candidate in source.iterdir():
                if candidate.is_file() and candidate.name.lower().startswith(PREFIXES):
                    shutil.copyfile(candidate, target/candidate.name)
            stated = p.get('license_file')
            if stated and (source/stated).is_file():
                shutil.copyfile(source/stated, target/Path(stated).name)
            for row in extra:
                if row['name'] != p['name'] or row['version'] != p['version']:
                    continue
                notice = target / row['notice']
                if notice.exists():
                    continue
                with urllib.request.urlopen(row['exact upstream URL'], timeout=15) as downloaded:
                    notice.write_bytes(downloaded.read())
            if not any(target.iterdir()):
                # Some crates carry declarations in their manifest or source headers only.
                for relative in ('Cargo.toml.orig', 'Cargo.toml', 'README.md', 'src/lib.rs', 'build.rs'):
                    candidate = source/relative
                    if candidate.is_file():
                        shutil.copyfile(candidate, target/Path(relative).name)
            inventory.writerow((p['name'], p['version'], p['license'] or p.get('license_file') or '',
                                p['repository'] or '', ', '.join(sorted(f.name for f in target.iterdir()))))


if __name__ == '__main__':
    main()
