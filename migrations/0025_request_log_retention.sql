-- Technical request envelopes only. Business/client/entity audit is untouched.
CREATE FUNCTION app.request_log_retention_cutoff() RETURNS timestamptz
LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $$
 SELECT (statement_timestamp() AT TIME ZONE 'Asia/Irkutsk' - interval '12 months') AT TIME ZONE 'Asia/Irkutsk'
$$;
REVOKE ALL ON FUNCTION app.request_log_retention_cutoff() FROM PUBLIC;

CREATE INDEX request_retention_finished ON app.api_request_log(finished_at,started_at,request_id)
 WHERE finished_at IS NOT NULL AND http_status IS NOT NULL;

CREATE OR REPLACE FUNCTION app.guard_request_log() RETURNS trigger LANGUAGE plpgsql
SET search_path=pg_catalog,pg_temp AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  -- current_user is the effective SECURITY DEFINER owner during the bounded
  -- function below. API cannot SET ROLE owner or spoof this through a GUC.
  IF current_user='colposcopy_owner' AND OLD.finished_at IS NOT NULL
     AND OLD.http_status IS NOT NULL
     AND OLD.finished_at<app.request_log_retention_cutoff()
     AND OLD.started_at<app.request_log_retention_cutoff() THEN
   RETURN OLD;
  END IF;
  RAISE EXCEPTION 'Request history is immutable' USING ERRCODE='55000';
 END IF;
 IF OLD.finished_at IS NOT NULL OR
    (to_jsonb(NEW)-ARRAY['finished_at','http_status','duration_ms','safe_error_code']) IS DISTINCT FROM
    (to_jsonb(OLD)-ARRAY['finished_at','http_status','duration_ms','safe_error_code']) THEN
  RAISE EXCEPTION 'Request history is immutable' USING ERRCODE='55000';
 END IF;
 RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION app.guard_request_log() FROM PUBLIC;

CREATE FUNCTION app.prune_api_request_log()
RETURNS TABLE(deleted_rows integer,skipped_busy boolean,cutoff timestamptz)
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp
SET lock_timeout='250ms' AS $$
DECLARE affected integer; boundary timestamptz:=app.request_log_retention_cutoff();
BEGIN
 IF NOT pg_try_advisory_xact_lock(530025) THEN
  RETURN QUERY SELECT 0,true,boundary;
  RETURN;
 END IF;
 WITH candidates AS MATERIALIZED (
  SELECT request_id FROM app.api_request_log
  WHERE finished_at<boundary AND started_at<boundary AND http_status IS NOT NULL
  ORDER BY finished_at,started_at,request_id LIMIT 1000 FOR UPDATE SKIP LOCKED
 ), removed AS (
  DELETE FROM app.api_request_log r USING candidates c WHERE r.request_id=c.request_id RETURNING 1
 ) SELECT count(*)::integer INTO affected FROM removed;
 RETURN QUERY SELECT affected,false,boundary;
END $$;
REVOKE ALL ON FUNCTION app.prune_api_request_log() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.prune_api_request_log() TO colposcopy_app;
-- No DELETE/TRUNCATE/owner membership is granted to the runtime role.
