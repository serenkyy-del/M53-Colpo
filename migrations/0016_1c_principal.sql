-- Called only after the server has verified this exact mapped 1C principal.
-- PostgreSQL cannot verify the upstream HTTP proof: the restricted API runtime
-- is the trust boundary. This helper never accepts passwords or arbitrary roles.
CREATE FUNCTION app.ensure_1c_principal(
 doctor_uuid uuid,
 org_uuid uuid,
 source_uuid uuid,
 user_guid uuid,
 expected_binding_version bigint
) RETURNS uuid
 LANGUAGE plpgsql SECURITY DEFINER
 SET search_path=pg_catalog
 SET lock_timeout='4s'
AS $$
DECLARE
 physician app.doctors%ROWTYPE;
 binding app.doctor_1c_logins%ROWTYPE;
 principal app.users%ROWTYPE;
 principal_uuid uuid;
BEGIN
 IF doctor_uuid IS NULL OR org_uuid IS NULL OR source_uuid IS NULL OR user_guid IS NULL
    OR doctor_uuid='00000000-0000-0000-0000-000000000000'::uuid
    OR org_uuid='00000000-0000-0000-0000-000000000000'::uuid
    OR source_uuid='00000000-0000-0000-0000-000000000000'::uuid
    OR user_guid='00000000-0000-0000-0000-000000000000'::uuid
    OR expected_binding_version IS NULL OR expected_binding_version<=0
    OR expected_binding_version=9223372036854775807 THEN
  RAISE EXCEPTION 'one_c_principal_denied' USING ERRCODE='42501';
 END IF;

 -- Match Core reconciliation's order: physician, binding, then M53 user.
 SELECT d.* INTO physician FROM app.doctors d
  WHERE d.id=doctor_uuid AND d.organization_id=org_uuid AND d.active FOR UPDATE;
 IF NOT FOUND THEN
  RAISE EXCEPTION 'one_c_principal_denied' USING ERRCODE='42501';
 END IF;
 SELECT b.* INTO binding FROM app.doctor_1c_logins b
  WHERE b.doctor_id=doctor_uuid AND b.organization_id=org_uuid FOR UPDATE;
 IF NOT FOUND OR NOT binding.enabled OR binding.source_id IS DISTINCT FROM source_uuid
    OR binding.user_guid IS DISTINCT FROM user_guid OR btrim(binding.login)='' THEN
  RAISE EXCEPTION 'one_c_principal_denied' USING ERRCODE='42501';
 END IF;
 IF binding.row_version<>expected_binding_version THEN
  RAISE EXCEPTION 'one_c_principal_binding_changed' USING ERRCODE='40001';
 END IF;
 PERFORM 1 FROM app.integration_sources s
  WHERE s.id=source_uuid AND s.organization_id=org_uuid AND s.enabled FOR SHARE;
 IF NOT FOUND THEN
  RAISE EXCEPTION 'one_c_principal_denied' USING ERRCODE='42501';
 END IF;

 SELECT u.* INTO principal FROM app.users u WHERE u.doctor_id=doctor_uuid FOR UPDATE;
 IF FOUND THEN
  IF principal.organization_id IS DISTINCT FROM org_uuid OR principal.disabled
     OR principal.auth_provider IS DISTINCT FROM '1c_web'
     OR principal.external_user_guid IS DISTINCT FROM user_guid
     OR NOT EXISTS(SELECT 1 FROM app.user_roles r
                   WHERE r.user_id=principal.id AND r.role_code='doctor') THEN
   -- Do not take over an M53 password account, re-enable a blocked user, or
   -- restore a role that an administrator explicitly removed.
   RAISE EXCEPTION 'one_c_principal_denied' USING ERRCODE='42501';
  END IF;
  RETURN principal.id;
 END IF;

 principal_uuid:=pg_catalog.gen_random_uuid();
 INSERT INTO app.users(id,organization_id,display_name,doctor_id,auth_provider,external_user_guid)
  VALUES(principal_uuid,org_uuid,physician.display_name,doctor_uuid,'1c_web',user_guid);
 INSERT INTO app.roles(code) VALUES('doctor') ON CONFLICT(code) DO NOTHING;
 INSERT INTO app.user_roles(user_id,role_code) VALUES(principal_uuid,'doctor');
 RETURN principal_uuid;
END $$;

REVOKE ALL ON FUNCTION app.ensure_1c_principal(uuid,uuid,uuid,uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.ensure_1c_principal(uuid,uuid,uuid,uuid,bigint) TO colposcopy_app;
COMMENT ON FUNCTION app.ensure_1c_principal(uuid,uuid,uuid,uuid,bigint) IS
 'Restricted runtime helper after verified 1C authentication; creates one passwordless physician principal and never restores revoked privileges.';
