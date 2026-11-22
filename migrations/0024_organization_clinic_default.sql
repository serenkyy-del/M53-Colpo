-- One synchronized clinic snapshot is the default for the whole organization.
-- Historic workstation bindings and immutable template versions are retained.
CREATE TABLE app.organization_clinic_defaults (
 organization_id uuid PRIMARY KEY REFERENCES app.organizations,
 branch_guid uuid NOT NULL,
 row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0),
 synced_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY(organization_id,branch_guid) REFERENCES app.clinic_branches(organization_id,guid)
);
GRANT SELECT,INSERT,UPDATE ON app.organization_clinic_defaults TO colposcopy_app;
CREATE TRIGGER organization_clinic_default_changed AFTER INSERT OR UPDATE OR DELETE
 ON app.organization_clinic_defaults FOR EACH ROW EXECUTE FUNCTION app.notify_template_change();
