#!/usr/bin/env python3
"""Verify public upstream pins, rebuild deterministic ZIP, emit new build manifest.
Does not modify the repository trust anchor. Copy the emitted manifest BEFORE
compiling the client; keep it and the archive as one immutable build input.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tempfile
import urllib.request
import zipfile

def digest(path):
 with Path(path).open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()

def safe(name):
 p=PurePosixPath(name)
 if not name or p.is_absolute() or any(v in ('.','..') for v in name.split('/')) or '\\' in name or ':' in name:raise ValueError('unsafe source path')
 return p

def pack(root,files,output):
 if len(files)>1000 or sum(f['size'] for f in files)>512*1024*1024:raise ValueError('runtime bounds exceeded')
 seen=set()
 for f in files:
  safe(f['path'])
  if f['path'] in seen:raise ValueError('duplicate path')
  seen.add(f['path']);path=root/f['path']
  if path.stat().st_size!=f['size'] or digest(path)!=f['sha256']:raise ValueError('runtime file fingerprint mismatch')
 with zipfile.ZipFile(output,'x',compression=zipfile.ZIP_STORED) as archive:
  for f in sorted(files,key=lambda f:f['path']):
   info=zipfile.ZipInfo(f['path'],date_time=(2026,1,1,0,0,0));info.create_system=3;info.external_attr=0o100644<<16
   archive.writestr(info,(root/f['path']).read_bytes(),compress_type=zipfile.ZIP_STORED)

def provision(manifest,cache,output):
 data=json.loads(manifest.read_text(encoding='utf-8-sig'))
 if data['format']!=1 or len(data['sources'])!=2:raise ValueError('unsupported runtime manifest')
 output.mkdir(parents=True,exist_ok=False);cache.mkdir(parents=True,exist_ok=True)
 sources=[]
 for source in data['sources']:
  if source['name'] not in ('chrome-headless-shell-win64.zip','pdfjs-6.4.299-dist.zip'):raise ValueError('unexpected upstream')
  path=cache/source['name']
  if not path.exists():
   if not source['url'].startswith(('https://storage.googleapis.com/chrome-for-testing-public/','https://github.com/mozilla/pdf.js/releases/')):raise ValueError('unexpected upstream URL')
   part=path.with_suffix('.partial')
   with urllib.request.urlopen(source['url'],timeout=120) as incoming,part.open('xb') as stream:shutil.copyfileobj(incoming,stream)
   part.rename(path)
  if path.stat().st_size!=source['size'] or digest(path)!=source['sha256']:raise ValueError('upstream archive fingerprint mismatch')
  sources.append(path)
 with tempfile.TemporaryDirectory(prefix='colposcopy-print-') as tmp:
  root=Path(tmp)
  with zipfile.ZipFile(sources[0]) as chrome,zipfile.ZipFile(sources[1]) as pdf:
   for f in data['files']:
    safe(f['path']);target=root/f['path'];target.parent.mkdir(parents=True,exist_ok=True)
    if f['path'].startswith('chrome/'):
     archive=chrome;name='chrome-headless-shell-win64/'+f['path'][7:]
    elif f['path'].startswith('pdfjs/'):
     archive=pdf;leaf=f['path'][6:];name=leaf if leaf=='LICENSE' else 'build/'+leaf
    else:raise ValueError('unexpected runtime file')
    entry=archive.getinfo(name)
    if entry.file_size!=f['size']:raise ValueError('source entry size mismatch')
    with archive.open(entry) as stream,target.open('xb') as dest:shutil.copyfileobj(stream,dest)
   destination=output/data['archive'];pack(root,data['files'],destination)
 data['archive_size']=destination.stat().st_size;data['archive_sha256']=digest(destination)
 (output/'manifest.json').write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
 print(json.dumps({'archive':str(destination),'archive_sha256':data['archive_sha256'],'manifest':str(output/'manifest.json')}))
if __name__=='__main__':
 parser=argparse.ArgumentParser();parser.add_argument('--manifest',type=Path,default=Path(__file__).resolve().parents[2]/'assets/print-runtime/manifest.json');parser.add_argument('--cache',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
 args=parser.parse_args();provision(args.manifest,args.cache,args.output)
