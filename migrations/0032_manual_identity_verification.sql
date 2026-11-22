CREATE TABLE app.manual_identity_verifications (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 study_id uuid NOT NULL, organization_id uuid NOT NULL, patient_id uuid NOT NULL,
 study_version bigint NOT NULL CHECK(study_version>0),
 patient_version bigint NOT NULL CHECK(patient_version>0),
 verified_by uuid NOT NULL, doctor_id uuid NOT NULL,
 verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(study_id,organization_id) REFERENCES app.studies(id,organization_id),
 FOREIGN KEY(patient_id,organization_id) REFERENCES app.patients(id,organization_id),
 FOREIGN KEY(verified_by,organization_id) REFERENCES app.users(id,organization_id),
 FOREIGN KEY(doctor_id,organization_id) REFERENCES app.doctors(id,organization_id)
);
CREATE INDEX manual_verification_study ON app.manual_identity_verifications(study_id,verified_by,patient_id,patient_version);
CREATE TRIGGER immutable_manual_verification BEFORE UPDATE OR DELETE ON app.manual_identity_verifications FOR EACH ROW EXECUTE FUNCTION app.guard_continuity_history();
GRANT SELECT,INSERT ON app.manual_identity_verifications TO colposcopy_app;
