#!/usr/bin/env python3
"""Portable audit of the same immutable runtime consumed by the client."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import sys
import zipfile

def verify_runtime(archive, manifest):
 data=json.loads(Path(manifest).read_text(encoding='utf-8-sig'))
 path=Path(archive)
 with path.open('rb') as stream: archive_digest=hashlib.file_digest(stream,'sha256').hexdigest()
 if data['format']!=1 or path.stat().st_size!=data['archive_size'] or archive_digest!=data['archive_sha256']:
  raise ValueError('runtime archive fingerprint mismatch')
 with zipfile.ZipFile(path) as source:
  expected={f['path']:f for f in data['files']}
  if len(expected)!=len(data['files']) or len(source.infolist())!=len(expected) or set(source.namelist())!=set(expected):raise ValueError('runtime entries differ')
  for name,record in expected.items():
   pure=PurePosixPath(name)
   if pure.is_absolute() or any(part in ('.','..') for part in pure.parts) or '\\' in name or ':' in name:raise ValueError('unsafe runtime path')
   item=source.getinfo(name)
   if item.file_size!=record['size']:raise ValueError('runtime entry size mismatch')
   with source.open(item) as stream:
    if hashlib.file_digest(stream,'sha256').hexdigest()!=record['sha256']:raise ValueError('runtime entry digest mismatch')
if __name__=='__main__':verify_runtime(Path(sys.argv[1]),Path(sys.argv[2]))
