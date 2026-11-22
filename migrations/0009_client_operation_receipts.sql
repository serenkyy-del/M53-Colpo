CREATE TABLE app.client_operations (
 credential_id uuid NOT NULL REFERENCES app.api_credentials,
 user_id uuid NOT NULL REFERENCES app.users,
 operation_id uuid NOT NULL,
 scope text NOT NULL,
 request_sha256 text NOT NULL CHECK(request_sha256 ~ '^[0-9a-f]{64}$'),
 response jsonb NOT NULL,
 received_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(user_id,operation_id)
);
GRANT SELECT,INSERT ON app.client_operations TO colposcopy_app;
