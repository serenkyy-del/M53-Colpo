GRANT USAGE ON SCHEMA app TO colposcopy_app;
GRANT SELECT, INSERT, UPDATE ON app.organizations, app.users, app.workstations, app.api_credentials, app.patients, app.patient_external_ids, app.orders, app.studies, app.protocol_revisions, app.lesions, app.findings, app.assets, app.annotation_sets, app.annotations TO colposcopy_app;
GRANT SELECT ON app.roles, app.user_roles, app.terminology_versions, app.terminology_terms TO colposcopy_app;
GRANT INSERT ON app.audit_events TO colposcopy_app;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA app FROM PUBLIC;
-- No blanket default DML: future tables require explicit reviewed grants.
ALTER DEFAULT PRIVILEGES FOR ROLE colposcopy_owner IN SCHEMA app REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
