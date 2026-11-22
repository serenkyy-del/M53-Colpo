-- Client-rendered PDFs are durable temporary delivery payloads, not original assets
-- or permanent report documents. Existing legacy rows retain their original FKs.
ALTER TABLE app.result_delivery_jobs ALTER COLUMN report_id DROP NOT NULL;
ALTER TABLE app.result_delivery_jobs ALTER COLUMN pdf_asset_id DROP NOT NULL;
ALTER TABLE app.result_delivery_jobs
 ADD COLUMN payload_kind text NOT NULL DEFAULT 'legacy_asset',
 ADD COLUMN cache_key text,
 ADD COLUMN pdf_byte_size bigint,
 ADD COLUMN protocol_revision_id uuid,
 ADD COLUMN template_version_id uuid REFERENCES app.report_template_versions,
 ADD COLUMN client_version text,
 ADD COLUMN renderer_version text,
 ADD COLUMN cache_state text;
ALTER TABLE app.result_delivery_jobs ADD CONSTRAINT result_payload_revision_study_fk
 FOREIGN KEY (protocol_revision_id,study_id) REFERENCES app.protocol_revisions(id,study_id);
ALTER TABLE app.result_delivery_jobs ADD CONSTRAINT result_payload_variant_check CHECK (
 (payload_kind='legacy_asset'
  AND report_id IS NOT NULL AND pdf_asset_id IS NOT NULL
  AND cache_key IS NULL AND pdf_byte_size IS NULL
  AND protocol_revision_id IS NULL AND template_version_id IS NULL
  AND client_version IS NULL AND renderer_version IS NULL AND cache_state IS NULL)
 OR
 (payload_kind='cache' AND report_id IS NULL AND pdf_asset_id IS NULL
  AND cache_key IS NOT NULL AND cache_key=replace(id::text,'-','')
  AND pdf_byte_size IS NOT NULL AND pdf_byte_size BETWEEN 1 AND 52428800
  AND protocol_revision_id IS NOT NULL AND template_version_id IS NOT NULL
  AND client_version IS NOT NULL AND btrim(client_version)<>'' AND octet_length(client_version)<=128
  AND renderer_version IS NOT NULL AND btrim(renderer_version)<>'' AND octet_length(renderer_version)<=128
  AND cache_state IS NOT NULL
  AND ((state='sent' AND cache_state IN ('cleanup_pending','purged'))
       OR (state<>'sent' AND cache_state='ready')))
);
CREATE INDEX result_payload_cleanup_idx ON app.result_delivery_jobs(created_at,id)
 WHERE payload_kind='cache' AND state='sent' AND cache_state='cleanup_pending';

-- Keep the existing trigger while extending every immutable snapshot field.
CREATE OR REPLACE FUNCTION app.guard_result_delivery_snapshot() RETURNS trigger
 LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.id,NEW.task_id,NEW.source_id,NEW.organization_id,NEW.application_key,
        NEW.study_id,NEW.publication_id,NEW.report_id,NEW.pdf_asset_id,NEW.pdf_sha256,
        NEW.patient_guid,NEW.appointment_date,NEW.sequence,NEW.created_at,
        NEW.payload_kind,NEW.cache_key,NEW.pdf_byte_size,NEW.protocol_revision_id,
        NEW.template_version_id,NEW.client_version,NEW.renderer_version)
    IS DISTINCT FROM
    ROW(OLD.id,OLD.task_id,OLD.source_id,OLD.organization_id,OLD.application_key,
        OLD.study_id,OLD.publication_id,OLD.report_id,OLD.pdf_asset_id,OLD.pdf_sha256,
        OLD.patient_guid,OLD.appointment_date,OLD.sequence,OLD.created_at,
        OLD.payload_kind,OLD.cache_key,OLD.pdf_byte_size,OLD.protocol_revision_id,
        OLD.template_version_id,OLD.client_version,OLD.renderer_version) THEN
  RAISE EXCEPTION 'Result delivery snapshot is immutable' USING ERRCODE='55000';
 END IF;
 IF OLD.payload_kind='cache' AND OLD.state='sent' AND NEW.state<>'sent' THEN
  RAISE EXCEPTION 'Acknowledged cached result cannot be delivered again' USING ERRCODE='55000';
 END IF;
 IF OLD.payload_kind='cache' AND OLD.cache_state='purged' AND NEW.cache_state<>'purged' THEN
  RAISE EXCEPTION 'Purged result cache cannot be restored' USING ERRCODE='55000';
 END IF;
 RETURN NEW;
END $$;
