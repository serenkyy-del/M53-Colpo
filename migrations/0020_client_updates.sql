CREATE TABLE app.client_releases (
 id uuid PRIMARY KEY, build_id text NOT NULL UNIQUE,
 build_sequence bigint NOT NULL UNIQUE CHECK(build_sequence>0),
 version text NOT NULL, platform text NOT NULL, channel text NOT NULL,
 min_api integer NOT NULL CHECK(min_api>0),max_api integer NOT NULL CHECK(max_api>=min_api),
 cache_format integer NOT NULL CHECK(cache_format>0),min_launcher integer NOT NULL CHECK(min_launcher>0),
 published_at bigint NOT NULL, expires_at bigint NOT NULL CHECK(expires_at>published_at),
 envelope jsonb NOT NULL CHECK(jsonb_typeof(envelope)='object'),
 manifest jsonb NOT NULL CHECK(jsonb_typeof(manifest)='object'),
 withdrawn_at timestamptz
);
CREATE TABLE app.client_update_policies (
 organization_id uuid NOT NULL REFERENCES app.organizations,
 release_id uuid NOT NULL REFERENCES app.client_releases,
 enabled boolean NOT NULL DEFAULT true,
 PRIMARY KEY(organization_id,release_id)
);
CREATE TABLE app.client_update_attempts (
 workstation_id uuid NOT NULL REFERENCES app.workstations,
 attempt_id uuid NOT NULL,release_id uuid NOT NULL REFERENCES app.client_releases,
 build_sequence bigint NOT NULL CHECK(build_sequence>0),
 state text NOT NULL CHECK(state IN ('staged','healthy','failed','rolled_back')),
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workstation_id,attempt_id)
);
CREATE TABLE app.client_update_events (
 workstation_id uuid NOT NULL,attempt_id uuid NOT NULL,
 state text NOT NULL CHECK(state IN ('staged','healthy','failed','rolled_back')),
 request_hash text NOT NULL,
 safe_error_code text,received_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workstation_id,attempt_id,state),
 FOREIGN KEY(workstation_id,attempt_id) REFERENCES app.client_update_attempts
);
CREATE TABLE app.workstation_update_state (
 workstation_id uuid PRIMARY KEY REFERENCES app.workstations,
 release_id uuid NOT NULL REFERENCES app.client_releases,
 attempt_id uuid NOT NULL,state text NOT NULL,
 last_healthy_sequence bigint,last_seen_at timestamptz NOT NULL DEFAULT now()
);
CREATE FUNCTION app.guard_client_release() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' OR (to_jsonb(NEW)-'withdrawn_at') IS DISTINCT FROM (to_jsonb(OLD)-'withdrawn_at')
    OR (OLD.withdrawn_at IS NOT NULL AND NEW.withdrawn_at IS DISTINCT FROM OLD.withdrawn_at) THEN
  RAISE EXCEPTION 'Published release is immutable';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_client_release BEFORE UPDATE OR DELETE ON app.client_releases
 FOR EACH ROW EXECUTE FUNCTION app.guard_client_release();
CREATE FUNCTION app.notify_update_policy() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_TABLE_NAME='client_releases' THEN
  PERFORM pg_notify('m53_update_changes',organization_id::text)
   FROM app.client_update_policies WHERE release_id=NEW.id;
 ELSIF TG_OP='DELETE' THEN
  PERFORM pg_notify('m53_update_changes',OLD.organization_id::text);
 ELSE
  PERFORM pg_notify('m53_update_changes',NEW.organization_id::text);
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER client_release_withdrawn AFTER UPDATE ON app.client_releases
 FOR EACH ROW EXECUTE FUNCTION app.notify_update_policy();
CREATE TRIGGER client_release_policy AFTER INSERT OR UPDATE OR DELETE ON app.client_update_policies
 FOR EACH ROW EXECUTE FUNCTION app.notify_update_policy();
REVOKE ALL ON FUNCTION app.guard_client_release(),app.notify_update_policy() FROM PUBLIC;
GRANT SELECT ON app.client_releases,app.client_update_policies TO colposcopy_app;
GRANT SELECT,INSERT,UPDATE ON app.client_update_attempts,app.workstation_update_state TO colposcopy_app;
GRANT SELECT,INSERT ON app.client_update_events TO colposcopy_app;
