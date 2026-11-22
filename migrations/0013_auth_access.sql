-- Own M53 access generations and metadata for 30-day signed offline grants.
-- Neither server passwords nor local password verifiers are stored in grants.
ALTER TABLE app.users ADD COLUMN auth_version bigint NOT NULL DEFAULT 1 CHECK(auth_version > 0);
ALTER TABLE app.workstations ADD CONSTRAINT workstations_id_org_unique UNIQUE(id,organization_id);
ALTER TABLE app.api_credentials ADD CONSTRAINT credentials_id_workstation_unique UNIQUE(id,workstation_id);

CREATE TABLE app.offline_access_grants (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 user_id uuid NOT NULL,
 doctor_id uuid NOT NULL,
 workstation_id uuid NOT NULL,
 credential_id uuid NOT NULL,
 auth_version bigint NOT NULL CHECK(auth_version > 0),
 key_id text NOT NULL CHECK(key_id ~ '^[0-9a-f]{64}$'),
 token_sha256 text NOT NULL CHECK(token_sha256 ~ '^[0-9a-f]{64}$'),
 issued_at timestamptz NOT NULL,
 expires_at timestamptz NOT NULL,
 revoked_at timestamptz,
 allow_new_manual_encounter boolean NOT NULL,
 allow_continue_open_encounter boolean NOT NULL,
 allow_local_print boolean NOT NULL,
 FOREIGN KEY(user_id,organization_id) REFERENCES app.users(id,organization_id),
 FOREIGN KEY(doctor_id,organization_id) REFERENCES app.doctors(id,organization_id),
 FOREIGN KEY(workstation_id,organization_id) REFERENCES app.workstations(id,organization_id),
 FOREIGN KEY(credential_id,workstation_id) REFERENCES app.api_credentials(id,workstation_id),
 CHECK(expires_at = issued_at + interval '720 hours'),
 CHECK(revoked_at IS NULL OR revoked_at >= issued_at)
);
CREATE INDEX offline_grants_user_idx ON app.offline_access_grants(user_id,auth_version);
CREATE INDEX offline_grants_credential_idx ON app.offline_access_grants(credential_id);
GRANT SELECT,INSERT,UPDATE ON app.offline_access_grants TO colposcopy_app;
REVOKE DELETE,TRUNCATE ON app.offline_access_grants FROM colposcopy_app;
GRANT SELECT ON app.schema_migrations TO colposcopy_app;

CREATE FUNCTION app.guard_offline_grant_metadata() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.id,NEW.organization_id,NEW.user_id,NEW.doctor_id,NEW.workstation_id,
        NEW.credential_id,NEW.auth_version,NEW.key_id,NEW.token_sha256,NEW.issued_at,
        NEW.expires_at,NEW.allow_new_manual_encounter,NEW.allow_continue_open_encounter,NEW.allow_local_print)
    IS DISTINCT FROM
    ROW(OLD.id,OLD.organization_id,OLD.user_id,OLD.doctor_id,OLD.workstation_id,
        OLD.credential_id,OLD.auth_version,OLD.key_id,OLD.token_sha256,OLD.issued_at,
        OLD.expires_at,OLD.allow_new_manual_encounter,OLD.allow_continue_open_encounter,OLD.allow_local_print)
    OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at) THEN
  RAISE EXCEPTION 'Offline grant metadata is immutable' USING ERRCODE='55000';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER offline_grant_metadata_immutable BEFORE UPDATE ON app.offline_access_grants
 FOR EACH ROW EXECUTE FUNCTION app.guard_offline_grant_metadata();

CREATE FUNCTION app.bump_user_access_fields() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF NEW.auth_version < OLD.auth_version THEN
  RAISE EXCEPTION 'Access version cannot decrease' USING ERRCODE='23514';
 END IF;
 IF ROW(NEW.disabled,NEW.doctor_id,NEW.organization_id)
    IS DISTINCT FROM ROW(OLD.disabled,OLD.doctor_id,OLD.organization_id)
    AND NEW.auth_version=OLD.auth_version THEN
  NEW.auth_version := OLD.auth_version+1;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER user_access_fields BEFORE UPDATE ON app.users
 FOR EACH ROW EXECUTE FUNCTION app.bump_user_access_fields();

CREATE FUNCTION app.revoke_changed_user_access() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF NEW.auth_version <> OLD.auth_version THEN
  UPDATE app.user_sessions SET revoked_at=clock_timestamp()
   WHERE user_id=NEW.id AND revoked_at IS NULL;
  UPDATE app.offline_access_grants SET revoked_at=clock_timestamp()
   WHERE user_id=NEW.id AND revoked_at IS NULL;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER revoke_changed_user_access AFTER UPDATE OF auth_version,disabled,doctor_id,organization_id ON app.users
 FOR EACH ROW EXECUTE FUNCTION app.revoke_changed_user_access();

CREATE FUNCTION app.bump_user_authentication() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=OLD.user_id;
  RETURN OLD;
 ELSIF TG_OP='INSERT' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=NEW.user_id;
 ELSIF ROW(NEW.user_id,NEW.login,NEW.password_hash)
       IS DISTINCT FROM ROW(OLD.user_id,OLD.login,OLD.password_hash) THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=NEW.user_id;
  IF NEW.user_id<>OLD.user_id THEN
   UPDATE app.users SET auth_version=auth_version+1 WHERE id=OLD.user_id;
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER user_authentication_changed AFTER INSERT OR UPDATE OR DELETE ON app.user_auth
 FOR EACH ROW EXECUTE FUNCTION app.bump_user_authentication();

CREATE FUNCTION app.bump_user_role_access() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=OLD.user_id;
  RETURN OLD;
 ELSIF TG_OP='INSERT' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=NEW.user_id;
 ELSIF ROW(NEW.user_id,NEW.role_code) IS DISTINCT FROM ROW(OLD.user_id,OLD.role_code) THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE id=NEW.user_id;
  IF NEW.user_id<>OLD.user_id THEN
   UPDATE app.users SET auth_version=auth_version+1 WHERE id=OLD.user_id;
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER user_role_access_changed AFTER INSERT OR UPDATE OR DELETE ON app.user_roles
 FOR EACH ROW EXECUTE FUNCTION app.bump_user_role_access();

CREATE FUNCTION app.bump_doctor_active_access() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF NEW.active IS DISTINCT FROM OLD.active THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE doctor_id=NEW.id;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER doctor_active_access_changed AFTER UPDATE OF active ON app.doctors
 FOR EACH ROW EXECUTE FUNCTION app.bump_doctor_active_access();

CREATE FUNCTION app.revoke_changed_workstation_access() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.workstation_id,NEW.purpose,NEW.verifier,NEW.permissions,NEW.expires_at,NEW.revoked_at)
    IS DISTINCT FROM ROW(OLD.workstation_id,OLD.purpose,OLD.verifier,OLD.permissions,OLD.expires_at,OLD.revoked_at) THEN
  UPDATE app.user_sessions SET revoked_at=clock_timestamp()
   WHERE credential_id=NEW.id AND revoked_at IS NULL;
  UPDATE app.offline_access_grants SET revoked_at=clock_timestamp()
   WHERE credential_id=NEW.id AND revoked_at IS NULL;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER workstation_access_changed AFTER UPDATE ON app.api_credentials
 FOR EACH ROW EXECUTE FUNCTION app.revoke_changed_workstation_access();

REVOKE ALL ON FUNCTION app.bump_user_access_fields(),app.revoke_changed_user_access(),
 app.bump_user_authentication(),app.bump_user_role_access(),app.bump_doctor_active_access(),
 app.revoke_changed_workstation_access(),app.guard_offline_grant_metadata() FROM PUBLIC;
