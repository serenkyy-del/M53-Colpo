ALTER TABLE app.patients ADD COLUMN sex text CHECK (sex IN ('F','M','O'));
CREATE OR REPLACE FUNCTION app.version_patient_identity() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF ROW(NEW.family_name,NEW.given_name,NEW.patronymic,NEW.birth_date,NEW.sex,
        NEW.organization_id,NEW.identity_state,NEW.origin)
    IS DISTINCT FROM
    ROW(OLD.family_name,OLD.given_name,OLD.patronymic,OLD.birth_date,OLD.sex,
        OLD.organization_id,OLD.identity_state,OLD.origin) THEN
  NEW.row_version:=OLD.row_version+1;
  NEW.updated_at:=now();
 ELSE
  NEW.row_version:=OLD.row_version;
  NEW.updated_at:=OLD.updated_at;
 END IF;
 RETURN NEW;
END $$;
-- Restore only unambiguous valid sex values already received for the exact GUID/source.
UPDATE app.patients p SET sex=known.sex
FROM (
 SELECT l.patient_id,min(t.patient_snapshot->>'sex') AS sex
 FROM app.patient_source_links l
 JOIN app.colposcopy_tasks t ON t.source_id=l.source_id
  AND t.organization_id=l.organization_id AND t.patient_guid=l.external_guid
 WHERE t.patient_snapshot->>'sex' IN ('F','M','O')
 GROUP BY l.patient_id HAVING count(DISTINCT t.patient_snapshot->>'sex')=1
) known WHERE p.id=known.patient_id AND p.sex IS NULL;
