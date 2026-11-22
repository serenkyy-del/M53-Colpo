-- Directory mapping only. Credentials/passwords from 1C are never imported.
CREATE TABLE app.doctor_1c_logins (
 doctor_id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 source_id uuid NOT NULL,
 user_guid uuid NOT NULL CHECK(user_guid<>'00000000-0000-0000-0000-000000000000'::uuid),
 login text NOT NULL CHECK(length(btrim(login)) BETWEEN 1 AND 256),
 enabled boolean NOT NULL,
 row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0),
 updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(source_id,user_guid),
 FOREIGN KEY(doctor_id,organization_id) REFERENCES app.doctors(id,organization_id),
 FOREIGN KEY(source_id,organization_id) REFERENCES app.integration_sources(id,organization_id),
 FOREIGN KEY(source_id,doctor_id) REFERENCES app.doctor_external_ids(source_id,doctor_id)
);
ALTER TABLE app.users ADD COLUMN auth_provider text NOT NULL DEFAULT 'm53'
 CHECK(auth_provider IN ('m53','1c_web'));
ALTER TABLE app.users ADD COLUMN external_user_guid uuid
 CHECK(external_user_guid IS NULL OR external_user_guid<>'00000000-0000-0000-0000-000000000000'::uuid);
ALTER TABLE app.user_sessions ADD COLUMN auth_provider text NOT NULL DEFAULT 'm53'
 CHECK(auth_provider IN ('m53','1c_web'));
ALTER TABLE app.user_sessions ADD COLUMN external_verified_at timestamptz;
ALTER TABLE app.user_sessions ADD CONSTRAINT session_external_verification
 CHECK((auth_provider='m53' AND external_verified_at IS NULL)
    OR (auth_provider='1c_web' AND external_verified_at IS NOT NULL));

CREATE FUNCTION app.version_1c_login_binding() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.source_id,NEW.user_guid,NEW.login,NEW.enabled)
    IS DISTINCT FROM ROW(OLD.source_id,OLD.user_guid,OLD.login,OLD.enabled) THEN
  NEW.row_version:=OLD.row_version+1;
  NEW.updated_at:=now();
 ELSE
  NEW.row_version:=OLD.row_version;
  NEW.updated_at:=OLD.updated_at;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER version_1c_login_binding BEFORE UPDATE ON app.doctor_1c_logins
 FOR EACH ROW EXECUTE FUNCTION app.version_1c_login_binding();

CREATE FUNCTION app.revoke_1c_login_binding() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE doctor_id=OLD.doctor_id;
  RETURN OLD;
 ELSIF TG_OP='INSERT' THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE doctor_id=NEW.doctor_id;
 ELSIF NEW.row_version<>OLD.row_version THEN
  UPDATE app.users SET auth_version=auth_version+1 WHERE doctor_id=NEW.doctor_id;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER revoke_1c_login_binding AFTER INSERT OR UPDATE OR DELETE ON app.doctor_1c_logins
 FOR EACH ROW EXECUTE FUNCTION app.revoke_1c_login_binding();

CREATE FUNCTION app.revoke_user_provider_change() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.auth_provider,NEW.external_user_guid)
    IS DISTINCT FROM ROW(OLD.auth_provider,OLD.external_user_guid) THEN
  NEW.auth_version:=OLD.auth_version+1;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER revoke_user_provider_change BEFORE UPDATE OF auth_provider,external_user_guid ON app.users
 FOR EACH ROW EXECUTE FUNCTION app.revoke_user_provider_change();
GRANT SELECT,INSERT,UPDATE ON app.doctor_1c_logins TO colposcopy_app;
REVOKE DELETE,TRUNCATE ON app.doctor_1c_logins FROM colposcopy_app;
