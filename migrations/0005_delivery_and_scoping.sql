ALTER TABLE app.report_templates ADD COLUMN organization_id uuid REFERENCES app.organizations;
UPDATE app.report_templates SET organization_id=(SELECT id FROM app.organizations ORDER BY id LIMIT 1) WHERE organization_id IS NULL;
ALTER TABLE app.delivery_jobs ADD COLUMN locked_at timestamptz;
CREATE UNIQUE INDEX one_publication_per_revision ON app.study_publications(protocol_revision_id);
CREATE TABLE app.login_attempts (credential_id uuid NOT NULL REFERENCES app.api_credentials, login_hash text NOT NULL, failures integer NOT NULL DEFAULT 0, blocked_until timestamptz, PRIMARY KEY(credential_id,login_hash));
GRANT SELECT,INSERT,UPDATE ON app.login_attempts TO colposcopy_app;
