#!/usr/bin/env python3
"""Synthetic standalone acceptance in a new disposable PG cluster; no external URL input.

Build the standalone server first. Optional PG_BINDIR and PSQL select local tools only.
The catalog authorization exercised here is solely for this disposable synthetic fixture.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from uuid import uuid4

ROOT = Path(__file__).resolve().parents[2]


def main():
    bindir = Path(os.environ.get('PG_BINDIR', '/usr/lib/postgresql/16/bin')).resolve()
    psql = Path(os.environ.get('PSQL', '/usr/lib/postgresql/16/bin/psql')).resolve()
    binary = ROOT / 'target/debug/m53-server'
    # Do not inherit credentials, service endpoints, libpq targets or integration flags.
    env = {k: v for k, v in os.environ.items() if not k.startswith(('M53_', 'PG'))}
    with tempfile.TemporaryDirectory(prefix='standalone-http-') as temporary:
        base = Path(temporary)
        sock = base / 'socket'
        sock.mkdir(mode=0o700)
        data = base / 'pgdata'

        def command(args, *, input=None, expected=0):
            result = subprocess.run([str(x) for x in args], input=input, text=True,
                                    env=env, cwd=ROOT, capture_output=True, timeout=60)
            assert result.returncode == expected, f'{Path(str(args[0])).name} {args[1:2]} failed: {result.stderr[-1800:]}'
            return result.stdout

        def sql(body, database='m53colpo'):
            return command([psql, '-X', '-At', '-v', 'ON_ERROR_STOP=1', '-h', sock,
                            '-p', '55438', '-U', 'synthetic_admin', '-d', database], input=body)

        command([bindir / 'initdb', '-D', data, '-U', 'synthetic_admin', '--auth-local=trust',
                 '--auth-host=reject', '--encoding=UTF8', '--no-locale'])
        started = False
        process = None
        try:
            command([bindir / 'pg_ctl', '-D', data, '-l', base / 'postgres.log',
                     '-o', f"-k {sock} -h '' -p 55438", '-w', 'start'])
            started = True
            sql('''CREATE ROLE colposcopy_owner NOLOGIN;
CREATE ROLE colposcopy_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
GRANT colposcopy_owner TO colposcopy_migrator;
CREATE ROLE colposcopy_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
CREATE DATABASE m53colpo OWNER colposcopy_owner;''', 'postgres')
            sql('''REVOKE ALL ON DATABASE m53colpo FROM PUBLIC;
GRANT CONNECT ON DATABASE m53colpo TO colposcopy_app, colposcopy_migrator;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SET ROLE colposcopy_owner;
CREATE SCHEMA app AUTHORIZATION colposcopy_owner;
CREATE TABLE app.schema_migrations(version text PRIMARY KEY, sha256 text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now());''')
            migration = command(['python3', ROOT / 'public/deploy/migration_sql.py'])
            sql(migration)
            sql(migration)  # same immutable fingerprints must be idempotent
            org, doctor, station = (str(uuid4()) for _ in range(3))
            password, key = secrets.token_urlsafe(24), secrets.token_urlsafe(40)
            for filename, value in [('password', password), ('workstation', key)]:
                path = base / filename
                path.write_text(value)
                path.chmod(0o600)
            connection = f'postgresql://colposcopy_migrator@localhost/m53colpo?host={sock}&port=55438'
            env.update(M53_DEPLOYMENT_DATABASE='m53colpo', M53_DATABASE_URL=connection,
                       M53_ORGANIZATION_ID=org, M53_ORGANIZATION_NAME='Synthetic independent clinic',
                       M53_DOCTOR_ID=doctor, M53_DOCTOR_NAME='Synthetic physician',
                       M53_USER_LOGIN='synthetic', M53_USER_PASSWORD_FILE=str(base / 'password'),
                       M53_WORKSTATION_ID=station, M53_WORKSTATION_NAME='Synthetic station',
                       M53_WORKSTATION_KEY_FILE=str(base / 'workstation'))
            command([binary, 'init-standalone'])
            command([binary, 'init-standalone'])
            assert sql('SELECT count(*) FROM app.patients;').strip() == '0'
            assert sql('SELECT count(*) FROM app.studies;').strip() == '0'
            assert sql('SELECT count(*) FROM app.users;').strip() == '1'
            print('PASS fresh migrations, repeated init, zero seeded clinical rows')

            version = str(uuid4())
            catalog = {'version_id': version, 'code': 'synthetic-only', 'source': 'Disposable test; not clinical terminology',
                       'terms': [{'code': 'synthetic_unused', 'region': 'cervix', 'kind': 'finding',
                                  'english': 'Synthetic unused fixture', 'russian': 'Синтетика', 'source_locator': 'test'}]}
            catalog_file = base / 'catalog.json'
            catalog_file.write_text(json.dumps(catalog))
            command([binary, 'import-terminology', catalog_file])
            review = base / 'review.json'
            review.write_text(json.dumps({'version_id': version, 'content_sha256': '0' * 64}))
            command([binary, 'approve-terminology', review], expected=2)
            assert sql('SELECT clinically_approved FROM app.terminology_versions;').strip() == 'f'
            print('PASS imported dictionary remains unapproved; wrong SHA authorization refused')

            env.update(M53_DATABASE_URL=connection.replace('colposcopy_migrator@', 'colposcopy_app@'),
                       M53_STORAGE_DIR=str(base / 'assets'), M53_CLIENT_RELEASES_DIR=str(base / 'releases'),
                       M53_DELIVERY_CACHE_DIR=str(base / 'delivery'), M53_OFFLINE_GRANT_DAYS='30',
                       M53_OFFLINE_ALLOW_NEW_ENCOUNTERS='1',
                       M53_OFFLINE_SIGNING_KEY_FILE=str(base / 'offline.pk8'),
                       M53_AUTH_PROVIDER='1c_web', M53_1C_RESULT_WORKER='1', M53_CORE_SYNC_WORKER='1',
                       M53_CORE_DOCTOR_SYNC_WORKER='1', M53_CORE_CLINIC_SYNC_WORKER='1', M53_DIAGNOSES_SYNC_WORKER='1')
            (base / 'delivery').mkdir()
            command([binary, 'generate-offline-key'])
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                port = listener.getsockname()[1]
            env['M53_LISTEN'] = f'127.0.0.1:{port}'
            target = f'http://127.0.0.1:{port}'
            auth = {'X-Workstation-Key': key, 'Content-Type': 'application/json'}

            def request(path, body=None, method=None, headers=None):
                assert path.startswith('/') and not path.startswith('//')
                h = {**auth, **(headers or {})}
                if body is not None:
                    h['X-Operation-ID'] = str(uuid4())
                req = Request(target + path, None if body is None else json.dumps(body).encode(),
                              headers=h, method=method)
                try:
                    response = urlopen(req, timeout=10)
                except HTTPError as error:
                    response = error
                with response:
                    raw = response.read()
                    try:
                        value = json.loads(raw)
                    except ValueError:
                        value = None
                    return response.status, value

            with (base / 'server.log').open('w') as log:
                process = subprocess.Popen([binary, 'serve'], cwd=ROOT, env=env, stdout=log, stderr=log)
                for _ in range(150):
                    assert process.poll() is None, 'standalone API exited before readiness'
                    try:
                        if request('/health')[0] == 200:
                            break
                    except OSError:
                        pass
                    time.sleep(.1)
                else:
                    raise AssertionError('standalone API readiness timeout')
                status, login = request('/api/v1/login', {'login': 'synthetic', 'password': password})
                assert status == 200, (status, login)
                auth['Authorization'] = 'Bearer ' + login['token']
                status, capabilities = request('/api/v1/capabilities')
                assert status == 200 and capabilities['auth_provider'] == 'm53'
                assert capabilities['auth_method'] == 'm53_password' and capabilities['own_auth']
                assert not capabilities['integration_1c'] and not capabilities['explicit_identity_link']
                for path in ['/api/v1/tasks', '/api/colposcopy/tasks', '/api/v1/auth/1c/login', '/api/v1/patients/external']:
                    assert request(path)[0] == 404, path
                print('PASS local login with misleading 1C auth/worker flags; integration routes closed')
                status, me = request('/api/v1/me')
                assert status == 200 and me['doctor']['id'] == doctor and me['offline_policy']['enabled']
                status, verification_key = request('/api/v1/auth/offline-key')
                assert status == 200
                status, grant = request('/api/v1/auth/offline-grant', {})
                assert status == 200 and grant['key'] == verification_key, (status, grant)
                _, payload, _ = grant['grant'].split('.')
                decoded = json.loads(base64.urlsafe_b64decode(payload + '=' * (-len(payload) % 4)))
                assert decoded['auth_provider'] == me['auth_provider'] == 'm53'
                assert decoded['sub'] == me['user_id'] and decoded['doctor_id'] == doctor
                assert decoded['workstation_id'] == station and decoded['credential_id'] == me['credential_id']
                assert decoded['exp'] - decoded['iat'] == 30 * 86400
                print('PASS signed standalone grant matches current local user and workstation')
                status, templates = request('/api/v1/templates', headers={'X-M53-Report-Format': 'letterhead-v1'})
                assert status == 200 and templates
                for template in templates:
                    header = template['layout']['letterhead']
                    assert header['logo_data_uri'] == ''
                    assert header['clinic']['name'] == 'Synthetic independent clinic'
                print('PASS neutral published template has no logo and uses own organization')
                status, patient = request('/api/v1/patients', {'family_name': 'Synthetic', 'given_name': 'Patient',
                                                              'patronymic': None, 'birth_date': '1990-01-01'},
                                          headers={'X-Entity-ID': str(uuid4())})
                assert status == 200, (status, patient)
                status, study = request('/api/v1/studies', {'patient_id': patient['id'], 'order_id': None, 'encounter_date': '2026-01-01'},
                                        headers={'X-Entity-ID': str(uuid4())})
                assert status == 200, (status, study)
                path = '/api/v1/studies/' + study['id']
                protocol = json.loads((ROOT / 'examples/protocol/draft.json').read_text())['protocol']
                protocol['terminology_version_id'] = version
                status, saved = request(path + '/protocol', {'expected_version': study['row_version'],
                                                              'protocol': protocol, 'annotations': []}, method='PUT')
                assert status == 200, (status, saved)
                context = request(path + '/context')[1]
                displayed = request(path)[1]['patient']
                verification = {'expected_version': saved['row_version'], 'patient_id': patient['id'],
                                'expected_patient_version': context['patient']['row_version'], 'confirmed': True,
                                'patient_data': {k: displayed.get(k) for k in ['family_name', 'given_name', 'patronymic', 'birth_date', 'sex']}}
                status, verified = request(path + '/manual-identity-verification', verification)
                assert status == 200, (status, verified)
                approval = {'study_id': study['id'], 'revision_id': saved['revision_id'], 'expected_version': saved['row_version']}
                status, refused = request(path + '/approve', approval)
                assert status == 400, (status, refused)
                assert sql("SELECT status FROM app.studies;").strip() == 'draft'
                print('PASS manual patient/study/protocol/identity verification; draft catalog prevents approval')
                env['M53_DATABASE_URL'] = connection
                review.write_text(json.dumps({'version_id': version, 'content_sha256': hashlib.sha256(catalog_file.read_bytes()).hexdigest()}))
                command([binary, 'approve-terminology', review])
                status, approved = request(path + '/approve', approval)
                assert status == 200, (status, approved)
                context = request(path + '/context')[1]
                assert context['approved_identity_snapshot']['identity_state'] == 'provisional'
                assert context['approved_identity_snapshot']['patient_snapshot']['manual_identity_verification']
                assert sql('SELECT count(*) FROM app.colposcopy_tasks;').strip() == '0'
                assert sql("SELECT (SELECT count(*) FROM app.patient_external_ids)+(SELECT count(*) FROM app.patient_source_links);").strip() == '0'
                assert sql('SELECT count(*) FROM app.result_delivery_jobs;').strip() == '0'
                print('PASS exact SHA review permits approval; no MIS identity/tasks/delivery jobs fabricated')
        finally:
            if process is not None:
                process.terminate()
                process.wait(timeout=15)
            if started:
                command([bindir / 'pg_ctl', '-D', data, '-m', 'immediate', '-w', 'stop'])


if __name__ == '__main__':
    main()
