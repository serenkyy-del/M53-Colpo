#!/bin/sh
set -eu
if [ "${DEPLOY_ADMIN:-0}" = 1 ]; then
  password=$(cat /run/secrets/migrator_password)
  role=colposcopy_migrator
  umask 077
  admin_secrets=$(mktemp -d)
  cp /run/secrets/doctor_password "$admin_secrets/password"
  cp /run/secrets/workstation_key "$admin_secrets/workstation"
  chmod 600 "$admin_secrets/password" "$admin_secrets/workstation"
  export M53_USER_PASSWORD_FILE="$admin_secrets/password"
  export M53_WORKSTATION_KEY_FILE="$admin_secrets/workstation"
else
  password=$(cat /run/secrets/app_password)
  role=colposcopy_app
fi
export M53_DATABASE_URL="postgresql://$role:$password@127.0.0.1:55432/m53colpo"
unset password
exec /usr/local/bin/server "$@"
