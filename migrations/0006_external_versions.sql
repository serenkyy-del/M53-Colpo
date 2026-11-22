ALTER TABLE app.patient_external_ids ADD COLUMN source_revision bigint NOT NULL DEFAULT 0;
ALTER TABLE app.patient_external_ids ADD COLUMN source_hash text NOT NULL DEFAULT '';
ALTER TABLE app.orders ADD COLUMN source_revision bigint NOT NULL DEFAULT 0;
ALTER TABLE app.orders ADD COLUMN source_hash text NOT NULL DEFAULT '';
