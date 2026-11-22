-- Versioned 1C-owned clinic details; workstation binding is explicit, never first-row.
ALTER TABLE app.patients ADD COLUMN card_number text CHECK(card_number IS NULL OR length(card_number) BETWEEN 1 AND 128);
ALTER TABLE app.report_template_versions ADD COLUMN clinic_branch_guid uuid;
CREATE TABLE app.clinic_branches (
 organization_id uuid NOT NULL REFERENCES app.organizations,
 guid uuid NOT NULL CHECK(guid<>'00000000-0000-0000-0000-000000000000'),
 name text NOT NULL CHECK(length(name) BETWEEN 1 AND 512),
 license text NOT NULL DEFAULT '' CHECK(length(license)<=4096),
 phone text NOT NULL DEFAULT '' CHECK(length(phone)<=1024),
 active boolean NOT NULL, source_version text, row_version bigint NOT NULL DEFAULT 1,
 synced_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(organization_id,guid)
);
ALTER TABLE app.workstations ADD CONSTRAINT workstation_identity_org UNIQUE(id,organization_id);
CREATE TABLE app.workstation_clinic_branches (
 workstation_id uuid PRIMARY KEY REFERENCES app.workstations,
 organization_id uuid NOT NULL REFERENCES app.organizations, branch_guid uuid NOT NULL,
 FOREIGN KEY(workstation_id,organization_id) REFERENCES app.workstations(id,organization_id),
 FOREIGN KEY(organization_id,branch_guid) REFERENCES app.clinic_branches(organization_id,guid)
);
GRANT SELECT,INSERT,UPDATE ON app.clinic_branches TO colposcopy_app;
GRANT SELECT ON app.workstation_clinic_branches TO colposcopy_app;
CREATE FUNCTION app.notify_template_change() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
DECLARE org uuid;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN RETURN NEW; END IF;
 IF TG_TABLE_NAME='report_template_versions' THEN
  SELECT organization_id INTO org FROM app.report_templates WHERE id=COALESCE(NEW.template_id,OLD.template_id);
 ELSE
  org:=COALESCE(NEW.organization_id,OLD.organization_id);
 END IF;
 IF org IS NOT NULL THEN PERFORM pg_notify('m53_template_changes',org::text); END IF;
 RETURN COALESCE(NEW,OLD);
END $$;
REVOKE ALL ON FUNCTION app.notify_template_change() FROM PUBLIC;
CREATE TRIGGER clinic_template_changed AFTER INSERT OR UPDATE OR DELETE ON app.clinic_branches FOR EACH ROW EXECUTE FUNCTION app.notify_template_change();
CREATE TRIGGER workstation_template_changed AFTER INSERT OR UPDATE OR DELETE ON app.workstation_clinic_branches FOR EACH ROW EXECUTE FUNCTION app.notify_template_change();
CREATE TRIGGER report_template_changed AFTER INSERT OR UPDATE OR DELETE ON app.report_template_versions FOR EACH ROW EXECUTE FUNCTION app.notify_template_change();

CREATE INDEX report_template_branch_latest ON app.report_template_versions(template_id,clinic_branch_guid,version DESC) WHERE published_at IS NOT NULL;
CREATE OR REPLACE FUNCTION app.version_patient_identity() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.family_name,NEW.given_name,NEW.patronymic,NEW.birth_date,NEW.sex,NEW.card_number,
        NEW.organization_id,NEW.identity_state,NEW.origin)
    IS DISTINCT FROM
    ROW(OLD.family_name,OLD.given_name,OLD.patronymic,OLD.birth_date,OLD.sex,OLD.card_number,
        OLD.organization_id,OLD.identity_state,OLD.origin) THEN
  NEW.row_version:=OLD.row_version+1;
  NEW.updated_at:=now();
 ELSE
  NEW.row_version:=OLD.row_version;
  NEW.updated_at:=OLD.updated_at;
 END IF;
 RETURN NEW;
END $$;
