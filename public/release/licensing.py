"""License texts and versioned upstream attribution for release packaging."""
import csv
import hashlib
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent
TEXTS = ROOT / 'license-texts'
SUPPLEMENTS = ROOT / 'license-supplements'
OPERATORS = {'AND', 'OR', 'WITH'}


def license_ids(expression):
    if not expression or not re.fullmatch(r'[A-Za-z0-9.()+ /\-]+', expression):
        raise ValueError(f'Invalid declared license: {expression!r}')
    ids = set(re.findall(r'[A-Za-z0-9.-]+', expression)) - OPERATORS
    if not ids:
        raise ValueError(f'Missing license identifier: {expression}')
    return sorted(ids)


def verify_text(path, expected):
    data = path.read_bytes().replace(b'\r\n', b'\n')
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f'License text fingerprint mismatch: {path}')


def load_catalog():
    with (TEXTS/'SOURCES.tsv').open(encoding='utf-8') as manifest:
        result = {}
        for row in csv.DictReader(manifest, delimiter='\t'):
            identifier = row['SPDX identifier']
            path = TEXTS/f'{identifier}.txt'
            verify_text(path, row['SHA-256 (LF)'])
            result[identifier] = path
    return result


def load_supplements():
    with (SUPPLEMENTS/'SOURCES.tsv').open(encoding='utf-8') as manifest:
        result = {}
        for row in csv.DictReader(manifest, delimiter='\t'):
            component = (row['name'], row['version'])
            path = SUPPLEMENTS/f'{component[0]}-{component[1]}'/row['notice']
            verify_text(path, row['SHA-256 (LF)'])
            result.setdefault(component, []).append(path)
    return result


def reference_files(expression, catalog):
    identifiers = license_ids(expression)
    missing = set(identifiers) - catalog.keys()
    if missing:
        raise ValueError(f'Unreviewed license text: {", ".join(sorted(missing))}')
    return [catalog[identifier] for identifier in identifiers]
