-- Immutable complete 1C catalogue generations; never part of clinical JSON.
CREATE TABLE app.diagnosis_catalog_versions (
 id uuid PRIMARY KEY,source_id uuid NOT NULL,organization_id uuid NOT NULL,
 content_sha256 text NOT NULL CHECK(content_sha256 ~ '^[0-9a-f]{64}$'),
 row_count bigint NOT NULL CHECK(row_count BETWEEN 1 AND 50000),
 published boolean NOT NULL DEFAULT false,
 synced_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(id,source_id,organization_id),
 FOREIGN KEY(source_id,organization_id) REFERENCES app.integration_sources(id,organization_id)
);
CREATE TABLE app.diagnosis_catalog_items (
 version_id uuid NOT NULL REFERENCES app.diagnosis_catalog_versions(id),
 guid uuid NOT NULL CHECK(guid<>'00000000-0000-0000-0000-000000000000'),
 code text NOT NULL CHECK(octet_length(code)<=32),
 name text NOT NULL CHECK(octet_length(name)<=4096),
 full_name text NOT NULL CHECK(octet_length(full_name)<=4096),
 active boolean NOT NULL,is_folder boolean NOT NULL,
 source_version text NOT NULL CHECK(octet_length(source_version)<=256),
 PRIMARY KEY(version_id,guid)
);
CREATE TABLE app.diagnosis_catalog_sync (
 source_id uuid PRIMARY KEY,organization_id uuid NOT NULL,
 current_version_id uuid,
 lease_id uuid,lease_until timestamptz,last_checked_at timestamptz,safe_error_code text,
 FOREIGN KEY(source_id,organization_id) REFERENCES app.integration_sources(id,organization_id),
 FOREIGN KEY(current_version_id,source_id,organization_id) REFERENCES app.diagnosis_catalog_versions(id,source_id,organization_id),
 CHECK((lease_id IS NULL)=(lease_until IS NULL))
);
-- Seal only once and only after all rows exist. Parent locking serializes
-- insert/seal, so later runtime INSERT cannot extend a published generation.
CREATE FUNCTION app.diagnosis_version_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='INSERT' AND NOT NEW.published THEN RETURN NEW; END IF;
 IF TG_OP='UPDATE' AND NOT OLD.published AND NEW.published
    AND to_jsonb(OLD)-'published'=to_jsonb(NEW)-'published'
    AND (SELECT count(*) FROM app.diagnosis_catalog_items WHERE version_id=NEW.id)=NEW.row_count
 THEN RETURN NEW; END IF;
 RAISE EXCEPTION 'diagnosis_catalog_immutable';
END $$;
CREATE TRIGGER diagnosis_versions_guard BEFORE INSERT OR UPDATE OR DELETE ON app.diagnosis_catalog_versions FOR EACH ROW EXECUTE FUNCTION app.diagnosis_version_guard();
CREATE FUNCTION app.diagnosis_item_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE sealed boolean;
BEGIN
 IF TG_OP='INSERT' THEN
  SELECT published INTO sealed FROM app.diagnosis_catalog_versions WHERE id=NEW.version_id FOR UPDATE;
  IF sealed=false THEN RETURN NEW; END IF;
 END IF;
 RAISE EXCEPTION 'diagnosis_catalog_immutable';
END $$;
CREATE TRIGGER diagnosis_items_guard BEFORE INSERT OR UPDATE OR DELETE ON app.diagnosis_catalog_items FOR EACH ROW EXECUTE FUNCTION app.diagnosis_item_guard();
GRANT SELECT,INSERT ON app.diagnosis_catalog_versions,app.diagnosis_catalog_items TO colposcopy_app;
GRANT UPDATE(published) ON app.diagnosis_catalog_versions TO colposcopy_app;
GRANT SELECT,INSERT,UPDATE ON app.diagnosis_catalog_sync TO colposcopy_app;
REVOKE DELETE,TRUNCATE ON app.diagnosis_catalog_sync,app.diagnosis_catalog_versions,app.diagnosis_catalog_items FROM colposcopy_app;
