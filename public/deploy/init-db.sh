#!/bin/sh
set -eu
app_password=$(cat /run/secrets/app_password)
migrator_password=$(cat /run/secrets/migrator_password)
psql -X -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" -v app_password="$app_password" -v migrator_password="$migrator_password" <<'SQL'
CREATE ROLE colposcopy_owner NOLOGIN;
CREATE ROLE colposcopy_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD :'app_password';
CREATE ROLE colposcopy_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD :'migrator_password';
GRANT colposcopy_owner TO colposcopy_migrator;
ALTER DATABASE m53colpo OWNER TO colposcopy_owner;
REVOKE ALL ON DATABASE m53colpo FROM PUBLIC;
GRANT CONNECT ON DATABASE m53colpo TO colposcopy_app, colposcopy_migrator;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SET ROLE colposcopy_owner;
CREATE SCHEMA app AUTHORIZATION colposcopy_owner;
REVOKE ALL ON SCHEMA app FROM PUBLIC;
CREATE TABLE app.schema_migrations(version text PRIMARY KEY, sha256 text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now());
SQL
