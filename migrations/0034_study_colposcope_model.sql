-- A study's equipment is captured at creation; legacy rows remain unknown.
ALTER TABLE app.studies ADD COLUMN colposcope_model text
 CHECK (colposcope_model IS NULL OR (octet_length(colposcope_model) BETWEEN 1 AND 256 AND btrim(colposcope_model) <> '' AND colposcope_model !~ '[[:cntrl:]]'));
CREATE FUNCTION app.guard_study_colposcope_model() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 RAISE EXCEPTION 'Study colposcope model is immutable' USING ERRCODE='55000';
END $$;
CREATE TRIGGER immutable_study_colposcope_model BEFORE UPDATE OF colposcope_model ON app.studies
 FOR EACH ROW WHEN (OLD.colposcope_model IS DISTINCT FROM NEW.colposcope_model)
 EXECUTE FUNCTION app.guard_study_colposcope_model();
REVOKE ALL ON FUNCTION app.guard_study_colposcope_model() FROM PUBLIC;
