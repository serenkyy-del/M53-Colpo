ALTER TABLE app.terminology_versions ADD COLUMN content_sha256 text NOT NULL DEFAULT '';
CREATE FUNCTION app.guard_terminology_term() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
DECLARE approved boolean;
BEGIN
 IF TG_OP IN ('UPDATE','DELETE') THEN
  SELECT clinically_approved INTO approved FROM app.terminology_versions WHERE id=OLD.version_id FOR UPDATE;
  IF approved THEN RAISE EXCEPTION 'Approved terminology is immutable'; END IF;
 END IF;
 IF TG_OP IN ('UPDATE','INSERT') THEN
  SELECT clinically_approved INTO approved FROM app.terminology_versions WHERE id=NEW.version_id FOR UPDATE;
  IF approved THEN RAISE EXCEPTION 'Approved terminology is immutable'; END IF;
  RETURN NEW;
 END IF;
 RETURN OLD;
END $$;
CREATE TRIGGER immutable_term BEFORE INSERT OR UPDATE OR DELETE ON app.terminology_terms FOR EACH ROW EXECUTE FUNCTION app.guard_terminology_term();
CREATE FUNCTION app.guard_terminology_version() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.clinically_approved THEN RAISE EXCEPTION 'Approved terminology is immutable'; END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_terminology BEFORE UPDATE OR DELETE ON app.terminology_versions FOR EACH ROW EXECUTE FUNCTION app.guard_terminology_version();
