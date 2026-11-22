-- Additive foundation for the direct 1C task API. No data import or demo seed.
-- application_key and task_name are opaque GUIDs from 1C, not M53 entity IDs.
-- Requests have no source version: serialize on (source, application_key),
-- compare only last_request_hash, and append a new receipt after any intervening
-- different request. A global UNIQUE(source, application_key, hash) is incorrect.

CREATE TABLE app.integration_sources (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL REFERENCES app.organizations,
 code text NOT NULL CHECK (btrim(code) <> ''),
 enabled boolean NOT NULL DEFAULT true,
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (organization_id, code),
 UNIQUE (id, organization_id)
);

CREATE TABLE app.integration_task_credentials (
 id uuid PRIMARY KEY,
 source_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 key_sha256 text NOT NULL UNIQUE CHECK (key_sha256 ~ '^[0-9a-f]{64}$'),
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz,
 revoked_at timestamptz,
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.integration_sources(id, organization_id),
 UNIQUE (id, source_id, organization_id),
 CHECK (expires_at IS NULL OR expires_at > created_at)
);

CREATE TABLE app.doctors (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL REFERENCES app.organizations,
 display_name text NOT NULL CHECK (btrim(display_name) <> ''),
 family_name text,
 given_name text,
 patronymic text,
 active boolean NOT NULL DEFAULT true,
 origin text NOT NULL CHECK (origin IN ('1c', 'local_pending_link')),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 row_version bigint NOT NULL DEFAULT 1 CHECK (row_version > 0),
 UNIQUE (id, organization_id)
);

CREATE TABLE app.doctor_external_ids (
 source_id uuid NOT NULL,
 external_guid uuid NOT NULL CHECK (external_guid <> '00000000-0000-0000-0000-000000000000'::uuid),
 organization_id uuid NOT NULL,
 doctor_id uuid NOT NULL,
 source_version text,
 source_hash text CHECK (source_hash IS NULL OR source_hash ~ '^[0-9a-f]{64}$'),
 last_seen_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (source_id, external_guid),
 UNIQUE (source_id, doctor_id),
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.integration_sources(id, organization_id),
 FOREIGN KEY (doctor_id, organization_id)
  REFERENCES app.doctors(id, organization_id)
);

ALTER TABLE app.users ADD COLUMN doctor_id uuid;
ALTER TABLE app.users ADD CONSTRAINT users_doctor_unique UNIQUE (doctor_id);
ALTER TABLE app.users ADD CONSTRAINT users_doctor_organization_fk
 FOREIGN KEY (doctor_id, organization_id) REFERENCES app.doctors(id, organization_id);
ALTER TABLE app.users ADD CONSTRAINT users_id_organization_unique UNIQUE (id, organization_id);

-- Existing nullable organizations are not guessed or backfilled. Only an
-- explicitly organization-bound patient can participate in a new source link.
ALTER TABLE app.patients ADD CONSTRAINT patients_id_organization_unique
 UNIQUE (id, organization_id);
CREATE TABLE app.patient_source_links (
 source_id uuid NOT NULL,
 external_guid uuid NOT NULL CHECK (external_guid <> '00000000-0000-0000-0000-000000000000'::uuid),
 organization_id uuid NOT NULL,
 patient_id uuid NOT NULL,
 source_version text,
 source_hash text CHECK (source_hash IS NULL OR source_hash ~ '^[0-9a-f]{64}$'),
 last_seen_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (source_id, external_guid),
 UNIQUE (source_id, patient_id),
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.integration_sources(id, organization_id),
 FOREIGN KEY (patient_id, organization_id)
  REFERENCES app.patients(id, organization_id)
);

ALTER TABLE app.studies ADD CONSTRAINT studies_id_patient_unique UNIQUE (id, patient_id);
CREATE TABLE app.colposcopy_tasks (
 id uuid PRIMARY KEY,
 source_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 application_key uuid NOT NULL CHECK (application_key <> '00000000-0000-0000-0000-000000000000'::uuid),
 task_name uuid NOT NULL CHECK (task_name <> '00000000-0000-0000-0000-000000000000'::uuid),
 appointment_date date NOT NULL,
 patient_guid uuid CHECK (patient_guid IS NULL OR patient_guid <> '00000000-0000-0000-0000-000000000000'::uuid),
 patient_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(patient_snapshot) = 'object'),
 state text NOT NULL DEFAULT 'waiting' CHECK (state IN ('waiting', 'in_progress', 'completed', 'cancelled')),
 assigned_doctor_id uuid,
 study_id uuid,
 study_patient_id uuid,
 row_version bigint NOT NULL DEFAULT 1 CHECK (row_version > 0),
 request_sequence bigint NOT NULL DEFAULT 0 CHECK (request_sequence >= 0),
 last_request_hash text CHECK (last_request_hash IS NULL OR last_request_hash ~ '^[0-9a-f]{64}$'),
 last_receipt_id uuid,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 cancelled_at timestamptz,
 UNIQUE (source_id, application_key),
 UNIQUE (id, source_id, organization_id, application_key),
 UNIQUE (id, study_id),
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.integration_sources(id, organization_id),
 FOREIGN KEY (assigned_doctor_id, organization_id)
  REFERENCES app.doctors(id, organization_id),
 FOREIGN KEY (study_id, study_patient_id)
  REFERENCES app.studies(id, patient_id),
 FOREIGN KEY (study_patient_id, organization_id)
  REFERENCES app.patients(id, organization_id),
 CHECK ((study_id IS NULL) = (study_patient_id IS NULL)),
 CHECK ((last_request_hash IS NULL) = (last_receipt_id IS NULL)),
 CHECK ((request_sequence = 0) = (last_receipt_id IS NULL)),
 CHECK ((state = 'cancelled') = (cancelled_at IS NOT NULL)),
 CHECK (state = 'cancelled' OR patient_guid IS NOT NULL)
);
CREATE INDEX colposcopy_tasks_worklist_idx
 ON app.colposcopy_tasks(organization_id, state, appointment_date, id);
CREATE INDEX colposcopy_tasks_doctor_idx
 ON app.colposcopy_tasks(organization_id, assigned_doctor_id, state, appointment_date, id);
CREATE UNIQUE INDEX colposcopy_tasks_source_study_unique
 ON app.colposcopy_tasks(source_id, study_id) WHERE study_id IS NOT NULL;

CREATE TABLE app.task_inbox (
 id uuid PRIMARY KEY,
 task_id uuid NOT NULL,
 source_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 application_key uuid NOT NULL,
 credential_id uuid NOT NULL,
 request_sequence bigint NOT NULL CHECK (request_sequence > 0),
 operation text NOT NULL CHECK (operation IN ('upsert', 'cancel')),
 request_hash text NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
 appointment_date date,
 payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
 response jsonb NOT NULL CHECK (jsonb_typeof(response) = 'object'),
 received_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (id, task_id),
 UNIQUE (id, task_id, request_sequence, request_hash),
 UNIQUE (task_id, request_sequence),
 FOREIGN KEY (task_id, source_id, organization_id, application_key)
  REFERENCES app.colposcopy_tasks(id, source_id, organization_id, application_key),
 FOREIGN KEY (credential_id, source_id, organization_id)
  REFERENCES app.integration_task_credentials(id, source_id, organization_id)
);
ALTER TABLE app.colposcopy_tasks ADD CONSTRAINT tasks_last_receipt_fk
 FOREIGN KEY (last_receipt_id, id, request_sequence, last_request_hash)
  REFERENCES app.task_inbox(id, task_id, request_sequence, request_hash)
 DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE app.task_events (
 id uuid PRIMARY KEY,
 task_id uuid NOT NULL,
 source_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 application_key uuid NOT NULL,
 inbox_id uuid,
 event_type text NOT NULL CHECK (btrim(event_type) <> ''),
 task_version bigint NOT NULL CHECK (task_version > 0),
 actor_user_id uuid,
 payload jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(payload) = 'object'),
 occurred_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY (task_id, source_id, organization_id, application_key)
  REFERENCES app.colposcopy_tasks(id, source_id, organization_id, application_key),
 FOREIGN KEY (inbox_id, task_id) REFERENCES app.task_inbox(id, task_id),
 FOREIGN KEY (actor_user_id, organization_id) REFERENCES app.users(id, organization_id)
);
CREATE INDEX task_events_task_idx ON app.task_events(task_id, occurred_at, id);

CREATE FUNCTION app.guard_task_append_only() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 RAISE EXCEPTION 'Task receipts and events are immutable' USING ERRCODE='55000';
END $$;
CREATE TRIGGER immutable_task_inbox BEFORE UPDATE OR DELETE ON app.task_inbox
 FOR EACH ROW EXECUTE FUNCTION app.guard_task_append_only();
CREATE TRIGGER immutable_task_event BEFORE UPDATE OR DELETE ON app.task_events
 FOR EACH ROW EXECUTE FUNCTION app.guard_task_append_only();

ALTER TABLE app.integration_settings DROP CONSTRAINT integration_settings_kind_check;
ALTER TABLE app.integration_settings ADD CONSTRAINT integration_settings_kind_check
 CHECK (kind IN ('orthanc', '1c', '1c_pdf', 'odata_core'));

-- A 1c_pdf configuration cannot be selected by the legacy kind='1c' HMAC hook.
-- Authentication secrets remain in integration_secrets and use existing rotation.
CREATE TABLE app.result_delivery_configs (
 source_id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 integration_id uuid NOT NULL UNIQUE REFERENCES app.integration_settings,
 enabled boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.integration_sources(id, organization_id),
 UNIQUE (source_id, organization_id)
);
CREATE FUNCTION app.guard_result_delivery_config_kind() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF NOT EXISTS (SELECT 1 FROM app.integration_settings s
                WHERE s.id=NEW.integration_id AND s.kind='1c_pdf') THEN
  RAISE EXCEPTION 'Result delivery requires a 1c_pdf integration' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER result_delivery_config_kind BEFORE INSERT OR UPDATE ON app.result_delivery_configs
 FOR EACH ROW EXECUTE FUNCTION app.guard_result_delivery_config_kind();
CREATE FUNCTION app.guard_linked_result_integration_kind() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF NEW.kind <> '1c_pdf' AND EXISTS
    (SELECT 1 FROM app.result_delivery_configs c WHERE c.integration_id=NEW.id) THEN
  RAISE EXCEPTION 'A linked result integration must remain 1c_pdf' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER linked_result_integration_kind BEFORE UPDATE OF kind ON app.integration_settings
 FOR EACH ROW EXECUTE FUNCTION app.guard_linked_result_integration_kind();

ALTER TABLE app.reports ADD CONSTRAINT reports_id_study_pdf_unique UNIQUE (id, study_id, pdf_asset_id);
ALTER TABLE app.assets ADD CONSTRAINT assets_id_sha256_unique UNIQUE (id, sha256);
CREATE TABLE app.result_delivery_jobs (
 id uuid PRIMARY KEY,
 task_id uuid NOT NULL,
 source_id uuid NOT NULL,
 organization_id uuid NOT NULL,
 application_key uuid NOT NULL,
 study_id uuid NOT NULL,
 publication_id uuid,
 report_id uuid NOT NULL,
 pdf_asset_id uuid NOT NULL,
 pdf_sha256 text NOT NULL CHECK (pdf_sha256 ~ '^[0-9a-f]{64}$'),
 patient_guid uuid NOT NULL CHECK (patient_guid <> '00000000-0000-0000-0000-000000000000'::uuid),
 appointment_date date NOT NULL,
 sequence bigint NOT NULL CHECK (sequence > 0),
 state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'retry', 'sent', 'blocked')),
 attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
 next_attempt_at timestamptz NOT NULL DEFAULT now(),
 receipt jsonb,
 safe_error_code text CHECK (safe_error_code IS NULL OR safe_error_code ~ '^[a-z0-9_]{1,128}$'),
 created_at timestamptz NOT NULL DEFAULT now(),
 sent_at timestamptz,
 locked_at timestamptz,
 lock_token uuid,
 UNIQUE (task_id, sequence),
 UNIQUE (task_id, pdf_sha256),
 FOREIGN KEY (task_id, source_id, organization_id, application_key)
  REFERENCES app.colposcopy_tasks(id, source_id, organization_id, application_key),
 FOREIGN KEY (task_id, study_id) REFERENCES app.colposcopy_tasks(id, study_id),
 FOREIGN KEY (source_id, organization_id)
  REFERENCES app.result_delivery_configs(source_id, organization_id),
 FOREIGN KEY (publication_id, study_id) REFERENCES app.study_publications(id, study_id),
 FOREIGN KEY (report_id, study_id, pdf_asset_id) REFERENCES app.reports(id, study_id, pdf_asset_id),
 FOREIGN KEY (pdf_asset_id, study_id) REFERENCES app.assets(id, study_id),
 FOREIGN KEY (pdf_asset_id, pdf_sha256) REFERENCES app.assets(id, sha256),
 CHECK ((state='sent') = (sent_at IS NOT NULL)),
 CHECK ((locked_at IS NULL) = (lock_token IS NULL))
);
CREATE INDEX result_delivery_jobs_pending_idx ON app.result_delivery_jobs(next_attempt_at, id)
 WHERE state IN ('pending', 'retry');

CREATE FUNCTION app.guard_result_delivery_snapshot() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.id,NEW.task_id,NEW.source_id,NEW.organization_id,NEW.application_key,
        NEW.study_id,NEW.publication_id,NEW.report_id,NEW.pdf_asset_id,NEW.pdf_sha256,
        NEW.patient_guid,NEW.appointment_date,NEW.sequence,NEW.created_at)
    IS DISTINCT FROM
    ROW(OLD.id,OLD.task_id,OLD.source_id,OLD.organization_id,OLD.application_key,
        OLD.study_id,OLD.publication_id,OLD.report_id,OLD.pdf_asset_id,OLD.pdf_sha256,
        OLD.patient_guid,OLD.appointment_date,OLD.sequence,OLD.created_at) THEN
  RAISE EXCEPTION 'Result delivery snapshot is immutable' USING ERRCODE='55000';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_result_delivery_snapshot BEFORE UPDATE ON app.result_delivery_jobs
 FOR EACH ROW EXECUTE FUNCTION app.guard_result_delivery_snapshot();

GRANT SELECT ON app.integration_sources,app.integration_task_credentials,app.result_delivery_configs TO colposcopy_app;
GRANT SELECT,INSERT,UPDATE ON app.doctors,app.doctor_external_ids,app.patient_source_links,app.colposcopy_tasks,app.result_delivery_jobs TO colposcopy_app;
GRANT SELECT,INSERT ON app.task_inbox,app.task_events TO colposcopy_app;
