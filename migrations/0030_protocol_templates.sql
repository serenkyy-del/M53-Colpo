-- Shared portable protocol templates; never store study/image identifiers.
CREATE TABLE app.protocol_templates (
 id uuid PRIMARY KEY CHECK(id <> '00000000-0000-0000-0000-000000000000'),
 organization_id uuid NOT NULL REFERENCES app.organizations,
 title text NOT NULL CHECK(char_length(title) BETWEEN 1 AND 200 AND btrim(title) <> ''),
 -- PostgreSQL's jsonb textual representation adds whitespace compared with
 -- the compact 4 MiB HTTP representation validated by the application.
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object' AND octet_length(payload::text)<=8388608),
 row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0),
 created_by uuid NOT NULL, updated_by uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(created_by,organization_id) REFERENCES app.users(id,organization_id),
 FOREIGN KEY(updated_by,organization_id) REFERENCES app.users(id,organization_id)
);
CREATE INDEX protocol_templates_org_id ON app.protocol_templates(organization_id,id);
CREATE FUNCTION app.guard_protocol_template() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'Protocol templates are retained' USING ERRCODE='55000'; END IF;
 IF TG_OP='UPDATE' THEN
  IF ROW(NEW.id,NEW.organization_id,NEW.created_by,NEW.created_at) IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.created_by,OLD.created_at) THEN RAISE EXCEPTION 'Protocol template identity is immutable' USING ERRCODE='55000'; END IF;
  NEW.row_version:=OLD.row_version+1;
 ELSE NEW.row_version:=1;
 END IF;
 NEW.updated_at:=clock_timestamp(); RETURN NEW;
END $$;
CREATE TRIGGER protocol_template_guard BEFORE INSERT OR UPDATE OR DELETE ON app.protocol_templates FOR EACH ROW EXECUTE FUNCTION app.guard_protocol_template();
CREATE TRIGGER audit_entity_change AFTER INSERT OR UPDATE ON app.protocol_templates FOR EACH ROW EXECUTE FUNCTION app.log_entity_change();
GRANT SELECT,INSERT,UPDATE ON app.protocol_templates TO colposcopy_app;
REVOKE ALL ON FUNCTION app.guard_protocol_template() FROM PUBLIC;
