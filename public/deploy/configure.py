#!/usr/bin/env python3
"""Generate deployment credentials once; never print them or replace a directory."""
import os
from pathlib import Path
import secrets
import sys

def configure(root):
    root.mkdir(mode=0o700, parents=False, exist_ok=False)
    for name in ('postgres_password', 'app_password', 'migrator_password', 'doctor_password', 'workstation_key'):
        with os.fdopen(os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
            stream.write(secrets.token_hex(32) + '\n')
if __name__ == '__main__':
    configure(Path(sys.argv[1]) if len(sys.argv) == 2 else Path(__file__).with_name('private'))
