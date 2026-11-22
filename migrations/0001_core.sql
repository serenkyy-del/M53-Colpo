CREATE TABLE app.organizations (id uuid PRIMARY KEY, name text NOT NULL);
CREATE TABLE app.users (id uuid PRIMARY KEY, organization_id uuid NOT NULL REFERENCES app.organizations, display_name text NOT NULL, disabled boolean NOT NULL DEFAULT false);
CREATE TABLE app.roles (code text PRIMARY KEY);
CREATE TABLE app.user_roles (user_id uuid REFERENCES app.users, role_code text REFERENCES app.roles, PRIMARY KEY(user_id,role_code));
CREATE TABLE app.workstations (id uuid PRIMARY KEY, organization_id uuid NOT NULL REFERENCES app.organizations, name text NOT NULL);
CREATE TABLE app.api_credentials (id uuid PRIMARY KEY, workstation_id uuid REFERENCES app.workstations, purpose text NOT NULL CHECK(purpose IN ('client','1c')), verifier text NOT NULL, permissions text[] NOT NULL, expires_at timestamptz, revoked_at timestamptz);
CREATE TABLE app.patients (id uuid PRIMARY KEY, family_name text NOT NULL, given_name text NOT NULL, patronymic text, birth_date date, created_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE app.patient_external_ids (patient_id uuid NOT NULL REFERENCES app.patients, source text NOT NULL, external_id text NOT NULL, dicom_issuer text NOT NULL, PRIMARY KEY(source,external_id));
CREATE TABLE app.orders (id uuid PRIMARY KEY, patient_id uuid NOT NULL REFERENCES app.patients, source text NOT NULL, external_order_id text NOT NULL, source_version text NOT NULL, status text NOT NULL CHECK(status IN ('active','cancelled','completed')), UNIQUE(source,external_order_id), UNIQUE(id,patient_id));
CREATE TABLE app.studies (id uuid PRIMARY KEY, patient_id uuid NOT NULL REFERENCES app.patients, order_id uuid, author_id uuid NOT NULL REFERENCES app.users, status text NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','review','approved','cancelled')), row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0), study_instance_uid text NOT NULL UNIQUE CHECK(length(study_instance_uid) BETWEEN 1 AND 64 AND study_instance_uid ~ '^[0-9]+(\.[0-9]+)+$'), created_at timestamptz NOT NULL DEFAULT now(), FOREIGN KEY(order_id,patient_id) REFERENCES app.orders(id,patient_id));
CREATE INDEX studies_patient_idx ON app.studies(patient_id);
CREATE TABLE app.terminology_versions (id uuid PRIMARY KEY, code text NOT NULL UNIQUE, source text NOT NULL, clinically_approved boolean NOT NULL DEFAULT false);
CREATE TABLE app.terminology_terms (version_id uuid NOT NULL REFERENCES app.terminology_versions, code text NOT NULL, region text NOT NULL CHECK(region IN ('cervix','vagina','vulva','anus','excision')), english text NOT NULL, russian text, source_locator text NOT NULL, PRIMARY KEY(version_id,code));
CREATE TABLE app.protocol_revisions (id uuid PRIMARY KEY, study_id uuid NOT NULL REFERENCES app.studies, revision integer NOT NULL CHECK(revision>0), terminology_version_id uuid NOT NULL REFERENCES app.terminology_versions, schema_version integer NOT NULL CHECK(schema_version>0), form jsonb NOT NULL CHECK(jsonb_typeof(form)='object'), conclusion text NOT NULL DEFAULT '', author_id uuid NOT NULL REFERENCES app.users, approved_by uuid REFERENCES app.users, approved_at timestamptz, CHECK((approved_by IS NULL)=(approved_at IS NULL)), UNIQUE(study_id,revision), UNIQUE(id,study_id), UNIQUE(id,study_id,terminology_version_id));
CREATE TABLE app.lesions (id uuid PRIMARY KEY, protocol_revision_id uuid NOT NULL, study_id uuid NOT NULL, region text NOT NULL CHECK(region IN ('cervix','vagina','vulva','anus')), location jsonb NOT NULL CHECK(jsonb_typeof(location)='object'), affected_percent numeric CHECK(affected_percent BETWEEN 0 AND 100), quadrants smallint CHECK(quadrants BETWEEN 0 AND 4), FOREIGN KEY(protocol_revision_id,study_id) REFERENCES app.protocol_revisions(id,study_id), UNIQUE(id,protocol_revision_id,study_id));
CREATE TABLE app.findings (id uuid PRIMARY KEY, study_id uuid NOT NULL, protocol_revision_id uuid NOT NULL, terminology_version_id uuid NOT NULL, lesion_id uuid, term_code text NOT NULL, state text NOT NULL CHECK(state IN ('present','absent','not_assessed','not_applicable')), FOREIGN KEY(protocol_revision_id,study_id,terminology_version_id) REFERENCES app.protocol_revisions(id,study_id,terminology_version_id), FOREIGN KEY(terminology_version_id,term_code) REFERENCES app.terminology_terms(version_id,code), FOREIGN KEY(lesion_id,protocol_revision_id,study_id) REFERENCES app.lesions(id,protocol_revision_id,study_id), UNIQUE(id,protocol_revision_id,study_id));
CREATE TABLE app.assets (id uuid PRIMARY KEY, study_id uuid NOT NULL REFERENCES app.studies, kind text NOT NULL CHECK(kind IN ('original','derived','html','pdf','dicom')), storage_key text NOT NULL UNIQUE CHECK(storage_key ~ '^[a-zA-Z0-9_-]+(/[a-zA-Z0-9_.-]+)*$' AND storage_key !~ '(^|/)\.\.?(/|$)'), sha256 text NOT NULL CHECK(sha256 ~ '^[0-9a-f]{64}$'), byte_size bigint NOT NULL CHECK(byte_size>=0), mime_type text NOT NULL, width integer CHECK(width>0), height integer CHECK(height>0), captured_at timestamptz, imported_at timestamptz NOT NULL DEFAULT now(), import_source text NOT NULL, parent_asset_id uuid, CHECK(parent_asset_id IS NULL OR parent_asset_id<>id), UNIQUE(id,study_id), FOREIGN KEY(parent_asset_id,study_id) REFERENCES app.assets(id,study_id));
CREATE INDEX assets_study_idx ON app.assets(study_id);
CREATE TABLE app.annotation_sets (id uuid PRIMARY KEY, study_id uuid NOT NULL, protocol_revision_id uuid NOT NULL, asset_id uuid NOT NULL, version integer NOT NULL CHECK(version>0), coordinate_system text NOT NULL CHECK(coordinate_system='original_normalized'), UNIQUE(id,protocol_revision_id,study_id), UNIQUE(protocol_revision_id,asset_id,version), FOREIGN KEY(protocol_revision_id,study_id) REFERENCES app.protocol_revisions(id,study_id), FOREIGN KEY(asset_id,study_id) REFERENCES app.assets(id,study_id));
CREATE TABLE app.annotations (id uuid PRIMARY KEY, annotation_set_id uuid NOT NULL, protocol_revision_id uuid NOT NULL, study_id uuid NOT NULL, lesion_id uuid, finding_id uuid, geometry jsonb NOT NULL CHECK(jsonb_typeof(geometry)='object'), display jsonb NOT NULL CHECK(jsonb_typeof(display)='object'), FOREIGN KEY(annotation_set_id,protocol_revision_id,study_id) REFERENCES app.annotation_sets(id,protocol_revision_id,study_id), FOREIGN KEY(lesion_id,protocol_revision_id,study_id) REFERENCES app.lesions(id,protocol_revision_id,study_id), FOREIGN KEY(finding_id,protocol_revision_id,study_id) REFERENCES app.findings(id,protocol_revision_id,study_id));
CREATE TABLE app.audit_events (id uuid PRIMARY KEY, actor_id uuid REFERENCES app.users, occurred_at timestamptz NOT NULL DEFAULT now(), action text NOT NULL, entity_id uuid NOT NULL, entity_version bigint, safe_description text NOT NULL);
CREATE FUNCTION app.guard_revision() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF OLD.approved_at IS NOT NULL THEN RAISE EXCEPTION 'Approved revision is immutable'; END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_revision BEFORE UPDATE OR DELETE ON app.protocol_revisions FOR EACH ROW EXECUTE FUNCTION app.guard_revision();
-- Lock the parent revision, serializing child writes with approval.
CREATE FUNCTION app.guard_revision_child() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
DECLARE revision_id uuid; approved timestamptz;
BEGIN
 IF TG_OP IN ('UPDATE','DELETE') THEN
  revision_id := OLD.protocol_revision_id;
  SELECT approved_at INTO approved FROM app.protocol_revisions WHERE id=revision_id FOR UPDATE;
  IF approved IS NOT NULL THEN RAISE EXCEPTION 'Approved revision children are immutable'; END IF;
 END IF;
 IF TG_OP IN ('UPDATE','INSERT') THEN
  revision_id := NEW.protocol_revision_id;
  SELECT approved_at INTO approved FROM app.protocol_revisions WHERE id=revision_id FOR UPDATE;
  IF approved IS NOT NULL THEN RAISE EXCEPTION 'Approved revision children are immutable'; END IF;
  RETURN NEW;
 END IF;
 RETURN OLD;
END $$;
CREATE TRIGGER immutable_lesion BEFORE INSERT OR UPDATE OR DELETE ON app.lesions FOR EACH ROW EXECUTE FUNCTION app.guard_revision_child();
CREATE TRIGGER immutable_finding BEFORE INSERT OR UPDATE OR DELETE ON app.findings FOR EACH ROW EXECUTE FUNCTION app.guard_revision_child();
CREATE TRIGGER immutable_annotation_set BEFORE INSERT OR UPDATE OR DELETE ON app.annotation_sets FOR EACH ROW EXECUTE FUNCTION app.guard_revision_child();
CREATE TRIGGER immutable_annotation BEFORE INSERT OR UPDATE OR DELETE ON app.annotations FOR EACH ROW EXECUTE FUNCTION app.guard_revision_child();
