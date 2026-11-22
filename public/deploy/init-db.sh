#!/bin/sh
set -eu
COLPOSCOPY_SETUP_APP_PASSWORD=$(cat /run/secrets/app_password)
COLPOSCOPY_SETUP_MIGRATOR_PASSWORD=$(cat /run/secrets/migrator_password)
export COLPOSCOPY_SETUP_APP_PASSWORD COLPOSCOPY_SETUP_MIGRATOR_PASSWORD
psql -X -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<'SQL'
\getenv app_password COLPOSCOPY_SETUP_APP_PASSWORD
\getenv migrator_password COLPOSCOPY_SETUP_MIGRATOR_PASSWORD
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
unset COLPOSCOPY_SETUP_APP_PASSWORD COLPOSCOPY_SETUP_MIGRATOR_PASSWORD
