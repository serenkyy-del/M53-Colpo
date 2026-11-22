-- Scoped, metadata-only access; do not grant the runtime role access to whole audit logs.
CREATE FUNCTION app.study_performer_creation(p_study uuid,p_org uuid)
RETURNS TABLE(actor_name text,doctor_name text,doctor_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,app AS $$
 SELECT e.actor_name,e.doctor_name,e.doctor_id
 FROM app.studies s JOIN app.patients p ON p.id=s.patient_id
 JOIN app.audit_events e ON e.entity_id=s.id AND e.action='study.created' AND e.actor_id=s.author_id AND e.organization_id=p_org
 WHERE s.id=p_study AND p.organization_id=p_org AND (s.organization_id IS NULL OR s.organization_id=p_org)
 ORDER BY e.occurred_at,e.id LIMIT 1;
$$;
CREATE FUNCTION app.protocol_revision_created_at(p_revision uuid,p_org uuid)
RETURNS timestamptz LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,app AS $$
 SELECT min(e.occurred_at)
 FROM app.protocol_revisions r JOIN app.studies s ON s.id=r.study_id JOIN app.patients p ON p.id=s.patient_id
 JOIN app.entity_change_events e ON e.entity_id=r.id AND e.entity_table='protocol_revisions' AND e.change_type='insert'
 WHERE r.id=p_revision AND p.organization_id=p_org AND (s.organization_id IS NULL OR s.organization_id=p_org);
$$;
REVOKE ALL ON FUNCTION app.study_performer_creation(uuid,uuid),app.protocol_revision_created_at(uuid,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.study_performer_creation(uuid,uuid),app.protocol_revision_created_at(uuid,uuid) TO colposcopy_app;
CREATE INDEX audit_study_creation_metadata ON app.audit_events(entity_id,organization_id,occurred_at,id) WHERE action='study.created';
CREATE INDEX revision_insert_metadata ON app.entity_change_events(entity_id,occurred_at) WHERE entity_table='protocol_revisions' AND change_type='insert';
INSERT INTO app.activity_actions(code,name,source) VALUES('server.patient.manual_updated','Изменены данные пациента ручного приёма','server_request');
