-- Provisional care is a real M53 encounter, never a fabricated 1C identity.
ALTER TABLE app.patients
 ADD COLUMN identity_state text NOT NULL DEFAULT 'provisional'
  CHECK (identity_state IN ('provisional','confirmed')),
 ADD COLUMN origin text NOT NULL DEFAULT 'legacy'
  CHECK (origin IN ('legacy','manual','1c')),
 ADD COLUMN row_version bigint NOT NULL DEFAULT 1 CHECK (row_version > 0),
 ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

-- Only existing, constrained GUID links establish a known external identity.
UPDATE app.patients p SET identity_state='confirmed',origin='1c'
 WHERE EXISTS(SELECT 1 FROM app.patient_source_links l WHERE l.patient_id=p.id
              AND l.organization_id=p.organization_id);

CREATE FUNCTION app.version_patient_identity() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.family_name,NEW.given_name,NEW.patronymic,NEW.birth_date,
        NEW.organization_id,NEW.identity_state,NEW.origin)
    IS DISTINCT FROM
    ROW(OLD.family_name,OLD.given_name,OLD.patronymic,OLD.birth_date,
        OLD.organization_id,OLD.identity_state,OLD.origin) THEN
  NEW.row_version:=OLD.row_version+1;
  NEW.updated_at:=now();
 ELSE
  NEW.row_version:=OLD.row_version;
  NEW.updated_at:=OLD.updated_at;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER patient_identity_version BEFORE UPDATE ON app.patients
 FOR EACH ROW EXECUTE FUNCTION app.version_patient_identity();

ALTER TABLE app.studies
 ADD COLUMN organization_id uuid REFERENCES app.organizations,
 ADD COLUMN assigned_doctor_id uuid,
 ADD COLUMN origin text NOT NULL DEFAULT 'legacy'
  CHECK (origin IN ('legacy','manual_continuity','1c_task')),
 ADD COLUMN encounter_date date,
 ADD CONSTRAINT studies_id_organization_unique UNIQUE (id,organization_id),
 ADD CONSTRAINT studies_patient_organization_fk FOREIGN KEY (patient_id,organization_id)
  REFERENCES app.patients(id,organization_id),
 ADD CONSTRAINT studies_doctor_organization_fk FOREIGN KEY (assigned_doctor_id,organization_id)
  REFERENCES app.doctors(id,organization_id),
 ADD CONSTRAINT continuity_study_context CHECK
  (origin='legacy' OR (organization_id IS NOT NULL AND assigned_doctor_id IS NOT NULL
                      AND encounter_date IS NOT NULL));
UPDATE app.studies s SET organization_id=p.organization_id
 FROM app.patients p WHERE p.id=s.patient_id AND p.organization_id IS NOT NULL;
UPDATE app.studies s SET assigned_doctor_id=t.assigned_doctor_id,
 origin=CASE WHEN t.assigned_doctor_id IS NULL THEN 'legacy' ELSE '1c_task' END,
 encounter_date=t.appointment_date
 FROM app.colposcopy_tasks t WHERE t.study_id=s.id
 AND NOT EXISTS(SELECT 1 FROM app.colposcopy_tasks other WHERE other.study_id=s.id
   AND ROW(other.assigned_doctor_id,other.appointment_date)
       IS DISTINCT FROM ROW(t.assigned_doctor_id,t.appointment_date));
CREATE INDEX continuity_studies_doctor_idx
 ON app.studies(organization_id,assigned_doctor_id,encounter_date,id);

CREATE TABLE app.identity_link_events (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 study_id uuid NOT NULL,
 source_patient_id uuid NOT NULL,
 target_patient_id uuid NOT NULL,
 source_id uuid NOT NULL,
 application_key uuid NOT NULL CHECK(application_key<>'00000000-0000-0000-0000-000000000000'::uuid),
 patient_guid uuid NOT NULL CHECK(patient_guid<>'00000000-0000-0000-0000-000000000000'::uuid),
 task_id uuid NOT NULL,
 doctor_id uuid NOT NULL,
 actor_user_id uuid NOT NULL,
 study_version bigint NOT NULL CHECK(study_version>0),
 task_version bigint NOT NULL CHECK(task_version>0),
 source_patient_version bigint NOT NULL CHECK(source_patient_version>0),
 target_patient_version bigint NOT NULL CHECK(target_patient_version>0),
 source_patient_snapshot jsonb NOT NULL CHECK(jsonb_typeof(source_patient_snapshot)='object'),
 target_patient_snapshot jsonb NOT NULL CHECK(jsonb_typeof(target_patient_snapshot)='object'),
 reason text NOT NULL CHECK(length(btrim(reason)) BETWEEN 1 AND 512),
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(study_id,study_version),
 FOREIGN KEY(study_id,organization_id) REFERENCES app.studies(id,organization_id),
 FOREIGN KEY(source_patient_id,organization_id) REFERENCES app.patients(id,organization_id),
 FOREIGN KEY(target_patient_id,organization_id) REFERENCES app.patients(id,organization_id),
 FOREIGN KEY(source_id,organization_id) REFERENCES app.integration_sources(id,organization_id),
 FOREIGN KEY(task_id,source_id,organization_id,application_key)
  REFERENCES app.colposcopy_tasks(id,source_id,organization_id,application_key),
 FOREIGN KEY(doctor_id,organization_id) REFERENCES app.doctors(id,organization_id),
 FOREIGN KEY(actor_user_id,organization_id) REFERENCES app.users(id,organization_id)
);

-- The clinical author, approver and identity printed at approval are snapshots.
-- No HTML/PDF bytes or permanent rendered report assets are introduced here.
CREATE TABLE app.revision_identity_snapshots (
 revision_id uuid PRIMARY KEY,
 study_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 patient_id uuid NOT NULL,
 identity_state text NOT NULL CHECK(identity_state IN ('provisional','confirmed')),
 patient_snapshot jsonb NOT NULL CHECK(jsonb_typeof(patient_snapshot)='object'),
 author_snapshot jsonb NOT NULL CHECK(jsonb_typeof(author_snapshot)='object'),
 approver_snapshot jsonb NOT NULL CHECK(jsonb_typeof(approver_snapshot)='object'),
 source_id uuid,
 application_key uuid,
 patient_guid uuid,
 created_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY(revision_id,study_id) REFERENCES app.protocol_revisions(id,study_id),
 FOREIGN KEY(study_id,organization_id) REFERENCES app.studies(id,organization_id),
 FOREIGN KEY(patient_id,organization_id) REFERENCES app.patients(id,organization_id),
 FOREIGN KEY(source_id,organization_id) REFERENCES app.integration_sources(id,organization_id),
 CHECK ((source_id IS NULL AND application_key IS NULL AND patient_guid IS NULL)
     OR (source_id IS NOT NULL AND application_key IS NOT NULL AND patient_guid IS NOT NULL))
);
CREATE FUNCTION app.guard_continuity_history() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 RAISE EXCEPTION 'Continuity identity history is immutable' USING ERRCODE='55000';
END $$;
CREATE TRIGGER immutable_identity_link_event BEFORE UPDATE OR DELETE ON app.identity_link_events
 FOR EACH ROW EXECUTE FUNCTION app.guard_continuity_history();
CREATE TRIGGER immutable_revision_identity_snapshot BEFORE UPDATE OR DELETE ON app.revision_identity_snapshots
 FOR EACH ROW EXECUTE FUNCTION app.guard_continuity_history();

CREATE FUNCTION app.guard_study_identity_change() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF OLD.organization_id IS NOT NULL AND NEW.organization_id IS DISTINCT FROM OLD.organization_id THEN
  RAISE EXCEPTION 'Study organization is immutable' USING ERRCODE='55000';
 END IF;
 IF NEW.patient_id IS DISTINCT FROM OLD.patient_id THEN
  IF OLD.origin<>'manual_continuity'
     OR current_setting('m53.identity_link_study',true) IS DISTINCT FROM OLD.id::text
     OR EXISTS(SELECT 1 FROM app.protocol_revisions WHERE study_id=OLD.id AND approved_at IS NOT NULL)
     OR EXISTS(SELECT 1 FROM app.study_publications WHERE study_id=OLD.id)
     OR EXISTS(SELECT 1 FROM app.result_delivery_jobs WHERE study_id=OLD.id)
     OR EXISTS(SELECT 1 FROM app.reports WHERE study_id=OLD.id) THEN
   RAISE EXCEPTION 'Study identity change requires explicit unapproved continuity link' USING ERRCODE='55000';
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER explicit_study_identity_change BEFORE UPDATE ON app.studies
 FOR EACH ROW EXECUTE FUNCTION app.guard_study_identity_change();

GRANT SELECT,INSERT ON app.identity_link_events,app.revision_identity_snapshots TO colposcopy_app;
