CREATE TABLE app.diagnosis_priority_groups (
 organization_id uuid PRIMARY KEY REFERENCES app.organizations(id),
 version bigint NOT NULL CHECK(version>0),
 diagnosis_ids uuid[] NOT NULL,
 updated_by uuid NOT NULL REFERENCES app.users(id),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
GRANT SELECT,INSERT,UPDATE ON app.diagnosis_priority_groups TO colposcopy_app;
REVOKE DELETE,TRUNCATE ON app.diagnosis_priority_groups FROM colposcopy_app;
