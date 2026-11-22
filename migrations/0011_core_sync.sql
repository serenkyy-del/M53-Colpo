-- Optional read-only enrichment from the already-running OData Core.
-- The signed 1C task snapshot and clinical patient data remain separate.
ALTER TABLE app.colposcopy_tasks ADD CONSTRAINT tasks_id_source_unique UNIQUE (id,source_id);
CREATE TABLE app.task_core_enrichment (
 task_id uuid PRIMARY KEY,
 source_id uuid NOT NULL REFERENCES app.integration_sources(id),
 patient_guid uuid,
 request_sequence bigint NOT NULL CHECK (request_sequence > 0),
 state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','running','retry','done','skipped')),
 attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
 lease_id uuid,
 lease_until timestamptz,
 next_attempt_at timestamptz NOT NULL DEFAULT now(),
 safe_error_code text,
 core_patient_snapshot jsonb CHECK (core_patient_snapshot IS NULL OR jsonb_typeof(core_patient_snapshot)='object'),
 source_version text,
 source_hash text CHECK (source_hash IS NULL OR source_hash ~ '^[0-9a-f]{64}$'),
 enriched_at timestamptz,
 updated_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY(task_id,source_id) REFERENCES app.colposcopy_tasks(id,source_id),
 CHECK ((lease_id IS NULL) = (lease_until IS NULL)),
 CHECK ((state='running') = (lease_id IS NOT NULL))
);
CREATE INDEX task_core_enrichment_ready_idx
 ON app.task_core_enrichment(source_id,state,next_attempt_at);

ALTER TABLE app.doctors ADD COLUMN source_details jsonb NOT NULL DEFAULT '{}'::jsonb
 CHECK (jsonb_typeof(source_details)='object');

-- Inbox snapshot hashes and Core tokens describe different payloads. Keep the
-- optional Core metadata separate instead of pairing its version with an
-- unrelated signed-task hash.
ALTER TABLE app.patient_source_links
 ADD COLUMN core_source_version text,
 ADD COLUMN core_source_hash text CHECK (core_source_hash IS NULL OR core_source_hash ~ '^[0-9a-f]{64}$'),
 ADD COLUMN core_last_seen_at timestamptz;

CREATE TABLE app.core_doctor_sync (
 source_id uuid PRIMARY KEY REFERENCES app.integration_sources(id),
 confirmed_skip bigint NOT NULL DEFAULT 0 CHECK (confirmed_skip BETWEEN 0 AND 4294967295),
 lease_id uuid,
 lease_until timestamptz,
 last_success_at timestamptz,
 safe_error_code text,
 updated_at timestamptz NOT NULL DEFAULT now(),
 CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);

GRANT SELECT,INSERT,UPDATE ON app.task_core_enrichment,app.core_doctor_sync TO colposcopy_app;
-- Preserve the same write policy as the underlying clinical/catalogue tables.
REVOKE DELETE,TRUNCATE ON app.task_core_enrichment,app.core_doctor_sync FROM colposcopy_app;
