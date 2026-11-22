CREATE TABLE app.notification_jobs (
 publication_id uuid PRIMARY KEY REFERENCES app.study_publications,
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','running','retry','complete')),
 attempts integer NOT NULL DEFAULT 0 CHECK(attempts>=0),
 next_attempt_at timestamptz NOT NULL DEFAULT now(),locked_at timestamptz,safe_error_code text
);
GRANT SELECT,INSERT,UPDATE ON app.notification_jobs TO colposcopy_app;
