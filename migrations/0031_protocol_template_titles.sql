-- Preserve historical duplicate templates while preventing new duplicates.
CREATE FUNCTION app.guard_protocol_template_title() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='UPDATE' AND NEW.title IS NOT DISTINCT FROM OLD.title THEN RETURN NEW; END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(NEW.organization_id::text || ':' || NEW.title, 0));
 IF EXISTS(SELECT 1 FROM app.protocol_templates t WHERE t.organization_id=NEW.organization_id AND t.title=NEW.title AND t.id<>NEW.id) THEN
  RAISE EXCEPTION 'protocol_template_title_conflict' USING ERRCODE='23505', CONSTRAINT='protocol_template_title_conflict';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER protocol_template_title_guard BEFORE INSERT OR UPDATE OF title ON app.protocol_templates FOR EACH ROW EXECUTE FUNCTION app.guard_protocol_template_title();
REVOKE ALL ON FUNCTION app.guard_protocol_template_title() FROM PUBLIC;
