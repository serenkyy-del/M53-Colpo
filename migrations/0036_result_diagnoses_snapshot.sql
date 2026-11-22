-- Existing jobs retain the exact legacy wire, even after feature activation.
-- No clinical forms/catalog rows or sent/retry jobs are backfilled.
CREATE FUNCTION app.valid_result_diagnoses_snapshot(value jsonb) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT SET search_path=pg_catalog,app AS $$
SELECT CASE WHEN jsonb_typeof(value)<>'array' THEN false ELSE
 jsonb_array_length(value)<=100 AND NOT EXISTS(
 SELECT 1 FROM jsonb_array_elements(value) AS entries(d)
 WHERE NOT coalesce(CASE WHEN jsonb_typeof(d)='object' THEN
  d ?& ARRAY['diagnosis_id','code','name'] AND d-ARRAY['diagnosis_id','code','name']='{}'::jsonb
  AND jsonb_typeof(d->'diagnosis_id')='string'
  AND d->>'diagnosis_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  AND d->>'diagnosis_id'<>'00000000-0000-0000-0000-000000000000'
  AND jsonb_typeof(d->'code')='string' AND octet_length(d->>'code') BETWEEN 1 AND 32 AND d->>'code' !~ '^[[:space:]]*$'
  AND jsonb_typeof(d->'name')='string' AND octet_length(d->>'name') BETWEEN 1 AND 4096 AND d->>'name' !~ '^[[:space:]]*$'
 ELSE false END,false)) END
$$;
GRANT EXECUTE ON FUNCTION app.valid_result_diagnoses_snapshot(jsonb) TO colposcopy_app;
ALTER TABLE app.result_delivery_jobs ADD COLUMN diagnoses_snapshot jsonb NOT NULL DEFAULT '[]'::jsonb
 CHECK(app.valid_result_diagnoses_snapshot(diagnoses_snapshot));
CREATE FUNCTION app.guard_result_diagnoses_snapshot() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
BEGIN
 IF NEW.diagnoses_snapshot IS DISTINCT FROM OLD.diagnoses_snapshot THEN
  RAISE EXCEPTION 'Result diagnoses snapshot is immutable' USING ERRCODE='55000';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_result_diagnoses_snapshot BEFORE UPDATE OF diagnoses_snapshot ON app.result_delivery_jobs
 FOR EACH ROW EXECUTE FUNCTION app.guard_result_diagnoses_snapshot();
