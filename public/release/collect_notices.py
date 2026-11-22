#!/usr/bin/env python3
"""Preserve bundled licenses and published metadata for the resolved crates."""
import argparse
import csv
import json
from pathlib import Path
import shutil
import urllib.request

from licensing import TEXTS, load_catalog, load_supplements, reference_files

PREFIXES = ('license', 'licence', 'notice', 'copying', 'copyright', 'unlicense', 'third-party')
UPSTREAM = Path(__file__).with_name('upstream-notices.tsv')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('metadata', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.mkdir(exist_ok=False, parents=True)
    catalog = load_catalog()
    supplements = load_supplements()
    shutil.copytree(TEXTS, args.output/'Reference-Licenses')
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
            declared = p.get('license')
            stated = p.get('license_file')
            stated_path = (source/stated).resolve() if stated else None
            if stated_path is not None:
                if not stated_path.is_relative_to(source.resolve()) or not stated_path.is_file():
                    raise ValueError(f"Missing published license file for {p['name']}")
                shutil.copyfile(stated_path, target/Path(stated).name)
            if not declared and not stated_path:
                raise ValueError(f"Missing license declaration for {p['name']}")
            refs = reference_files(declared, catalog) if declared else []
            for notice in supplements.get((p['name'], p['version']), []):
                if not (target/notice.name).exists():
                    shutil.copyfile(notice, target/notice.name)
            for row in extra:
                if row['name'] != p['name'] or row['version'] != p['version']:
                    continue
                notice = target / row['notice']
                if notice.exists():
                    continue
                with urllib.request.urlopen(row['exact upstream URL'], timeout=15) as downloaded:
                    notice.write_bytes(downloaded.read())
            if not any(path.name.lower().startswith(('licen', 'copying', 'unlicense')) for path in target.iterdir()):
                # The Cargo publication may declare a standard license without carrying its separate text.
                for reference in refs:
                    shutil.copyfile(reference, target/f'LICENSE-{reference.name}')
                for relative in ('Cargo.toml.orig', 'Cargo.toml', 'README.md', 'src/lib.rs', 'build.rs'):
                    candidate = source/relative
                    if candidate.is_file():
                        shutil.copyfile(candidate, target/Path(relative).name)
            attribution = [
                f"{p['name']} {p['version']}",
                f"Published license declaration: {declared or stated}",
                f"Upstream: {p['repository'] or 'see the published Cargo archive'}",
                *[f"Published author: {author}" for author in p.get('authors', [])],
            ]
            (target/'NOTICE-PACKAGE.txt').write_text('\n'.join(attribution)+'\n', encoding='utf-8')
            inventory.writerow((p['name'], p['version'], declared or stated,
                                p['repository'] or '', ', '.join(sorted(f.name for f in target.iterdir()))))


if __name__ == '__main__':
    main()
