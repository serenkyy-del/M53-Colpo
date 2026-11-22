#!/usr/bin/env python3
"""Emit atomic per-file migrations; mismatch aborts, repeated hashes skip."""
import hashlib
from pathlib import Path
root = Path(__file__).resolve().parents[2]
print('SET ROLE colposcopy_owner;')
for path in sorted((root / 'migrations').glob('*.sql')):
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    version = path.stem
    print(f"""BEGIN;
LOCK TABLE app.schema_migrations IN SHARE ROW EXCLUSIVE MODE;
SELECT EXISTS(SELECT 1 FROM app.schema_migrations WHERE version='{version}') AS applied \\gset
\\if :applied
DO $$ BEGIN IF NOT EXISTS(SELECT 1 FROM app.schema_migrations WHERE version='{version}' AND sha256='{digest}') THEN RAISE EXCEPTION 'migration fingerprint mismatch'; END IF; END $$;
\\else
{path.read_text()}
INSERT INTO app.schema_migrations(version,sha256) VALUES('{version}','{digest}');
\\endif
COMMIT;""")
