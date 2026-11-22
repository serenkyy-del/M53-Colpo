-- PostgreSQL delivers NOTIFY only after commit; rolled-back changes are invisible.
-- No patient data, task identifiers, or bearer tokens in the channel payload.
CREATE FUNCTION app.notify_client_change() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
DECLARE org uuid;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN RETURN NEW; END IF;
 IF TG_OP='DELETE' THEN org:=OLD.organization_id; ELSE org:=NEW.organization_id; END IF;
 IF org IS NOT NULL THEN PERFORM pg_notify('m53_client_changes',org::text); END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END $$;
REVOKE ALL ON FUNCTION app.notify_client_change() FROM PUBLIC;
CREATE TRIGGER client_patient_changed AFTER INSERT OR UPDATE OR DELETE ON app.patients
 FOR EACH ROW EXECUTE FUNCTION app.notify_client_change();
CREATE TRIGGER client_task_changed AFTER INSERT OR UPDATE OR DELETE ON app.colposcopy_tasks
 FOR EACH ROW EXECUTE FUNCTION app.notify_client_change();
CREATE TRIGGER client_study_changed AFTER INSERT OR UPDATE OR DELETE ON app.studies
 FOR EACH ROW EXECUTE FUNCTION app.notify_client_change();
CREATE TRIGGER client_delivery_changed AFTER INSERT OR UPDATE OR DELETE ON app.result_delivery_jobs
 FOR EACH ROW EXECUTE FUNCTION app.notify_client_change();
