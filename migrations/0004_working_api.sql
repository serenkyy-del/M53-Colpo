ALTER TABLE app.patients ADD COLUMN organization_id uuid REFERENCES app.organizations;
CREATE INDEX patients_org_idx ON app.patients(organization_id);
ALTER TABLE app.terminology_terms ADD COLUMN kind text NOT NULL DEFAULT 'finding' CHECK(kind IN ('finding','anatomy','test'));
CREATE TABLE app.user_auth (user_id uuid PRIMARY KEY REFERENCES app.users, login text NOT NULL UNIQUE, password_hash text NOT NULL);
CREATE TABLE app.user_sessions (token_hash text PRIMARY KEY, user_id uuid NOT NULL REFERENCES app.users, credential_id uuid NOT NULL REFERENCES app.api_credentials, expires_at timestamptz NOT NULL, revoked_at timestamptz);
CREATE UNIQUE INDEX credential_verifier_idx ON app.api_credentials(verifier);
CREATE TABLE app.protocol_annotations (revision_id uuid PRIMARY KEY REFERENCES app.protocol_revisions, data jsonb NOT NULL CHECK(jsonb_typeof(data)='array'));
CREATE FUNCTION app.guard_protocol_annotations() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
DECLARE approved timestamptz;
BEGIN
 IF TG_OP IN ('UPDATE','DELETE') THEN
  SELECT approved_at INTO approved FROM app.protocol_revisions WHERE id=OLD.revision_id FOR UPDATE;
  IF approved IS NOT NULL THEN RAISE EXCEPTION 'Approved annotations are immutable'; END IF;
 END IF;
 IF TG_OP IN ('UPDATE','INSERT') THEN
  SELECT approved_at INTO approved FROM app.protocol_revisions WHERE id=NEW.revision_id FOR UPDATE;
  IF approved IS NOT NULL THEN RAISE EXCEPTION 'Approved annotations are immutable'; END IF;
  RETURN NEW;
 END IF;
 RETURN OLD;
END $$;
CREATE TRIGGER immutable_protocol_annotations BEFORE INSERT OR UPDATE OR DELETE ON app.protocol_annotations FOR EACH ROW EXECUTE FUNCTION app.guard_protocol_annotations();
GRANT SELECT ON app.user_auth TO colposcopy_app;
GRANT SELECT,INSERT,UPDATE ON app.user_sessions,app.protocol_annotations TO colposcopy_app;
-- Each saved draft is a new revision; approved snapshots are never overwritten.
CREATE INDEX revision_latest_idx ON app.protocol_revisions(study_id,revision DESC);
