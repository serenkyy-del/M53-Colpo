-- Preserve legacy uncertainty: old rows have no invented creation times or actors.
ALTER TABLE app.workstations ADD COLUMN created_at timestamptz,
 ADD COLUMN updated_at timestamptz,ADD COLUMN row_version bigint NOT NULL DEFAULT 1,
 ADD COLUMN disabled_at timestamptz;
ALTER TABLE app.workstations ALTER COLUMN created_at SET DEFAULT clock_timestamp();
ALTER TABLE app.api_credentials ADD COLUMN name text NOT NULL DEFAULT 'Ключ рабочего места',
 ADD COLUMN created_at timestamptz,ADD COLUMN disabled_reason text;
ALTER TABLE app.api_credentials ALTER COLUMN created_at SET DEFAULT clock_timestamp();
UPDATE app.api_credentials SET name='Ключ '||left(id::text,8) WHERE purpose='client';
CREATE UNIQUE INDEX unique_client_key_verifier ON app.api_credentials(verifier) WHERE purpose='client';

CREATE TABLE app.workstation_registry_events (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 organization_id uuid REFERENCES app.organizations,workstation_id uuid REFERENCES app.workstations,
 credential_id uuid REFERENCES app.api_credentials,action text NOT NULL,
 workstation_name text,key_name text,key_fingerprint text,disabled boolean,disabled_reason text,
 sql_actor text NOT NULL DEFAULT current_user,operator_label text,
 database_txid bigint NOT NULL DEFAULT txid_current()
);
CREATE FUNCTION app.audit_workstation_registry() RETURNS trigger LANGUAGE plpgsql
 SECURITY DEFINER SET search_path=pg_catalog,app AS $$
DECLARE ws uuid; org uuid; label text; event text;
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'Workstations and client keys are retained; disable instead' USING ERRCODE='55000'; END IF;
 IF TG_TABLE_NAME='workstations' THEN
  IF TG_OP='UPDATE' AND (NEW.id IS DISTINCT FROM OLD.id OR NEW.organization_id IS DISTINCT FROM OLD.organization_id OR NEW.created_at IS DISTINCT FROM OLD.created_at) THEN
   RAISE EXCEPTION 'Workstation identity is immutable' USING ERRCODE='55000';
  END IF;
  IF TG_OP='UPDATE' AND ROW(NEW.name,NEW.disabled_at) IS NOT DISTINCT FROM ROW(OLD.name,OLD.disabled_at) THEN RETURN NEW; END IF;
  IF TG_OP='UPDATE' THEN NEW.row_version:=OLD.row_version+1; END IF;
  NEW.updated_at:=clock_timestamp(); ws:=NEW.id;org:=NEW.organization_id;label:=NEW.name;
  -- BEFORE INSERT cannot reference the new row by FK; history is appended by AFTER below.
  RETURN NEW;
 ELSE
  IF TG_OP='UPDATE' AND ROW(NEW.id,NEW.workstation_id,NEW.purpose,NEW.verifier,NEW.created_at) IS DISTINCT FROM ROW(OLD.id,OLD.workstation_id,OLD.purpose,OLD.verifier,OLD.created_at) THEN
   RAISE EXCEPTION 'Key identity and verifier are immutable; issue another key' USING ERRCODE='55000';
  END IF;
  RETURN NEW;
 END IF;
END $$;
CREATE TRIGGER guard_workstation_registry BEFORE INSERT OR UPDATE OR DELETE ON app.workstations FOR EACH ROW EXECUTE FUNCTION app.audit_workstation_registry();
CREATE TRIGGER guard_credential_registry BEFORE UPDATE OR DELETE ON app.api_credentials FOR EACH ROW EXECUTE FUNCTION app.audit_workstation_registry();
CREATE FUNCTION app.record_workstation_registry() RETURNS trigger LANGUAGE plpgsql
 SECURITY DEFINER SET search_path=pg_catalog,app AS $$
DECLARE ws uuid; org uuid; label text; event text;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN RETURN NULL; END IF;
 IF TG_TABLE_NAME='workstations' THEN
  ws:=NEW.id;org:=NEW.organization_id;label:=NEW.name;
  event:=CASE WHEN TG_OP='INSERT' THEN 'workstation.created' WHEN NEW.disabled_at IS NOT NULL THEN 'workstation.disabled' ELSE 'workstation.changed' END;
  INSERT INTO app.workstation_registry_events(organization_id,workstation_id,action,workstation_name,disabled,sql_actor,operator_label)
   VALUES(org,ws,event,label,NEW.disabled_at IS NOT NULL,session_user,nullif(current_setting('m53.operator_label',true),''));
  IF TG_OP='UPDATE' AND NEW.disabled_at IS NOT NULL AND OLD.disabled_at IS NULL THEN
   UPDATE app.user_sessions SET revoked_at=clock_timestamp() WHERE credential_id IN(SELECT id FROM app.api_credentials WHERE workstation_id=ws) AND revoked_at IS NULL;
   UPDATE app.offline_access_grants SET revoked_at=clock_timestamp() WHERE workstation_id=ws AND revoked_at IS NULL;
  END IF;
 ELSE
  IF NEW.purpose<>'client' THEN RETURN NULL; END IF;
  SELECT w.id,w.organization_id,w.name INTO ws,org,label FROM app.workstations w WHERE w.id=NEW.workstation_id;
  event:=CASE WHEN TG_OP='INSERT' THEN 'credential.issued' WHEN NEW.revoked_at IS NOT NULL THEN 'credential.disabled' WHEN OLD.revoked_at IS NOT NULL THEN 'credential.enabled' ELSE 'credential.changed' END;
  INSERT INTO app.workstation_registry_events(organization_id,workstation_id,credential_id,action,workstation_name,key_name,key_fingerprint,disabled,disabled_reason,sql_actor,operator_label)
   VALUES(org,ws,NEW.id,event,label,NEW.name,left(NEW.verifier,12),NEW.revoked_at IS NOT NULL,NEW.disabled_reason,session_user,nullif(current_setting('m53.operator_label',true),''));
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER workstation_registry_record AFTER INSERT OR UPDATE ON app.workstations FOR EACH ROW EXECUTE FUNCTION app.record_workstation_registry();
CREATE TRIGGER credential_registry_record AFTER INSERT OR UPDATE ON app.api_credentials FOR EACH ROW EXECUTE FUNCTION app.record_workstation_registry();
INSERT INTO app.workstation_registry_events(organization_id,workstation_id,credential_id,action,workstation_name,key_name,key_fingerprint,disabled)
 SELECT w.organization_id,w.id,c.id,'registry.baseline',w.name,c.name,left(c.verifier,12),c.revoked_at IS NOT NULL FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.purpose='client';

ALTER TABLE app.audit_events ADD COLUMN organization_id uuid REFERENCES app.organizations,
 ADD COLUMN workstation_id uuid REFERENCES app.workstations,ADD COLUMN credential_id uuid REFERENCES app.api_credentials,
 ADD COLUMN doctor_id uuid REFERENCES app.doctors,ADD COLUMN workstation_name text,ADD COLUMN key_name text,
 ADD COLUMN actor_name text,ADD COLUMN doctor_name text,ADD COLUMN database_txid bigint,
 ADD COLUMN request_id uuid,ADD COLUMN operation_id uuid,
 ADD COLUMN context_quality text NOT NULL DEFAULT 'legacy_unknown';
ALTER TABLE app.audit_events ALTER COLUMN database_txid SET DEFAULT txid_current();
CREATE FUNCTION app.snapshot_audit_actor() RETURNS trigger LANGUAGE plpgsql
 SET search_path=pg_catalog,app AS $$
DECLARE user_org uuid;
BEGIN
 IF NEW.credential_id IS NOT NULL THEN
  SELECT w.id,w.organization_id,w.name,c.name INTO NEW.workstation_id,NEW.organization_id,NEW.workstation_name,NEW.key_name
   FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.id=NEW.credential_id;
 END IF;
 IF NEW.actor_id IS NOT NULL THEN
  SELECT u.organization_id,u.display_name,u.doctor_id,d.display_name INTO user_org,NEW.actor_name,NEW.doctor_id,NEW.doctor_name
   FROM app.users u LEFT JOIN app.doctors d ON d.id=u.doctor_id WHERE u.id=NEW.actor_id;
  IF NEW.organization_id IS NOT NULL AND user_org IS DISTINCT FROM NEW.organization_id THEN RAISE EXCEPTION 'Audit actor organization mismatch'; END IF;
  NEW.organization_id:=coalesce(NEW.organization_id,user_org);
 END IF;
 NEW.database_txid:=txid_current();
 NEW.context_quality:=CASE WHEN NEW.credential_id IS NOT NULL THEN 'server_verified' ELSE 'server_partial' END;
 RETURN NEW;
END $$;
CREATE TRIGGER audit_actor_snapshot BEFORE INSERT ON app.audit_events FOR EACH ROW EXECUTE FUNCTION app.snapshot_audit_actor();
CREATE FUNCTION app.deny_audit_rewrite() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$ BEGIN RAISE EXCEPTION 'Audit history is append-only' USING ERRCODE='55000'; END $$;
CREATE TRIGGER immutable_audit_events BEFORE UPDATE OR DELETE ON app.audit_events FOR EACH ROW EXECUTE FUNCTION app.deny_audit_rewrite();
CREATE TRIGGER immutable_registry_events BEFORE UPDATE OR DELETE ON app.workstation_registry_events FOR EACH ROW EXECUTE FUNCTION app.deny_audit_rewrite();
CREATE INDEX audit_workstation_time ON app.audit_events(workstation_id,occurred_at);
CREATE INDEX audit_actor_time ON app.audit_events(actor_id,occurred_at);
CREATE INDEX audit_transaction ON app.audit_events(database_txid);

CREATE TABLE app.activity_actions(code text PRIMARY KEY,name text NOT NULL,source text NOT NULL CHECK(source IN ('client','server_request')));
INSERT INTO app.activity_actions(code,name,source) VALUES
('client.app.started','Программа запущена','client'),
('client.app.closed','Программа закрыта','client'),
('client.auth.offline_login','Автономный вход врача','client'),
('client.auth.logout_requested','Запрошен выход','client'),
('client.patient.selected','Выбран пациент','client'),
('client.tasks.refresh_requested','Запрошено обновление заданий','client'),
('client.study.opened','Открыто обследование','client'),
('client.study.closed','Закрыто обследование','client'),
('client.image.import_started','Начат импорт снимков','client'),
('client.image.import_completed','Импорт снимков завершен','client'),
('client.image.import_failed','Ошибка импорта снимков','client'),
('client.image.capture_requested','Запрошен захват снимка','client'),
('client.image.capture_completed','Снимок получен','client'),
('client.image.selected','Выбран снимок','client'),
('client.image.zoom_changed','Изменен масштаб снимка','client'),
('client.image.pan_changed','Снимок перемещен в окне','client'),
('client.annotation.added','Добавлена метка','client'),
('client.annotation.moved','Перемещена метка','client'),
('client.annotation.resized','Изменен размер метки','client'),
('client.annotation.removed','Удалена метка','client'),
('client.annotation.retagged','Изменено обозначение метки','client'),
('client.target.changed','Изменена мишень','client'),
('client.report_region.changed','Изменена область снимка для отчета','client'),
('client.protocol.field_changed','Изменено поле протокола','client'),
('client.protocol.section_opened','Раскрыт раздел протокола','client'),
('client.protocol.save_clicked','Нажато сохранение протокола','client'),
('client.protocol.approve_clicked','Нажато утверждение протокола','client'),
('client.protocol.reopen_clicked','Запрошена новая редакция','client'),
('client.report.previewed','Открыт предпросмотр отчета','client'),
('client.report.pdf_generated','Сформирован PDF','client'),
('client.report.print_requested','Открыта печать отчета','client'),
('client.report.print_dialog_closed','Закрыт диалог печати','client'),
('client.report.send_clicked','Нажато отправить в МИС','client'),
('client.settings.opened','Открыты настройки','client'),
('client.settings.changed','Изменена настройка','client'),
('client.sync.started','Начата синхронизация','client'),
('client.sync.finished','Синхронизация завершена','client'),
('client.sync.failed','Ошибка синхронизации','client'),
('client.sync.conflict_detected','Обнаружен конфликт версий','client'),
('client.sync.conflict_resolved','Врач разрешил конфликт','client'),
('client.camera.connected','Камера подключена','client'),
('client.camera.disconnected','Камера отключена','client'),
('client.pedal.capture_pressed','Нажата педаль захвата','client'),
('api.request','Вызов API','server_request'),
('api.patients.read','Просмотр списка пациентов','server_request'),
('api.studies.read','Просмотр обследования или списка','server_request'),
('api.tasks.read','Просмотр заданий','server_request'),
('api.assets.read','Получение снимка','server_request'),
('api.auth.login','Попытка входа','server_request'),
('api.auth.logout','Выход','server_request'),
('api.workstation.read','Получение настроек рабочего места','server_request'),
('api.activity.submit','Передача журнала действий','server_request'),
('api.updates.check','Проверка обновления','server_request'),
('api.updates.download','Загрузка обновления','server_request'),
('api.updates.status','Статус обновления','server_request'),
('api.events.subscribe','Подключение уведомлений','server_request'),
('api.clinical.write','Изменение клинических данных','server_request'),
('api.report.read','Получение отчета или статуса доставки','server_request'),
('api.catalog.read','Получение справочника','server_request');
CREATE TABLE app.api_request_log (
 request_id uuid PRIMARY KEY,organization_id uuid REFERENCES app.organizations,
 workstation_id uuid REFERENCES app.workstations,credential_id uuid REFERENCES app.api_credentials,
 integration_credential_id uuid REFERENCES app.integration_task_credentials,
 user_id uuid REFERENCES app.users,doctor_id uuid REFERENCES app.doctors,
 workstation_name text,key_name text,user_name text,doctor_name text,
 action_code text NOT NULL REFERENCES app.activity_actions,action_name text NOT NULL,credential_active boolean NOT NULL,
 method text NOT NULL,route text NOT NULL,operation_id uuid,requested_entity_id uuid,
 started_at timestamptz NOT NULL DEFAULT clock_timestamp(),finished_at timestamptz,
 safe_error_code text,http_status smallint CHECK(http_status BETWEEN 100 AND 599),duration_ms bigint CHECK(duration_ms>=0)
);
CREATE INDEX request_workstation_time ON app.api_request_log(workstation_id,started_at);
CREATE INDEX request_actor_time ON app.api_request_log(user_id,started_at);
CREATE TABLE app.client_activity_events (
 event_id uuid NOT NULL,workstation_id uuid NOT NULL REFERENCES app.workstations,
 credential_id uuid NOT NULL REFERENCES app.api_credentials,reported_credential_id uuid REFERENCES app.api_credentials,
 organization_id uuid NOT NULL REFERENCES app.organizations,user_id uuid REFERENCES app.users,
 doctor_id uuid REFERENCES app.doctors,workstation_name text NOT NULL,key_name text NOT NULL,
 user_name text,doctor_name text,
 action_code text NOT NULL REFERENCES app.activity_actions,action_name text NOT NULL,
 reported_at bigint NOT NULL,received_at timestamptz NOT NULL DEFAULT clock_timestamp(),client_sequence bigint NOT NULL CHECK(client_sequence>0),
 study_id uuid REFERENCES app.studies,entity_id uuid,operation_id uuid,field_code text,
 request_hash text NOT NULL,PRIMARY KEY(workstation_id,event_id)
);
CREATE INDEX client_activity_actor_time ON app.client_activity_events(user_id,received_at);
CREATE INDEX client_activity_study_time ON app.client_activity_events(study_id,received_at);
CREATE TRIGGER immutable_client_activity BEFORE UPDATE OR DELETE ON app.client_activity_events FOR EACH ROW EXECUTE FUNCTION app.deny_audit_rewrite();
CREATE TABLE app.entity_change_events (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 database_txid bigint NOT NULL DEFAULT txid_current(),entity_table text NOT NULL,entity_id uuid,
 change_type text NOT NULL,state_before text,state_after text,changed_fields text[] NOT NULL
);
CREATE FUNCTION app.log_entity_change() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,app AS $$
DECLARE before_row jsonb;after_row jsonb; changed text[]; entity uuid;
BEGIN
 before_row:=CASE WHEN TG_OP='INSERT' THEN '{}'::jsonb ELSE to_jsonb(OLD) END;
 after_row:=CASE WHEN TG_OP='DELETE' THEN '{}'::jsonb ELSE to_jsonb(NEW) END;
 SELECT array_agg(key ORDER BY key) INTO changed FROM (SELECT jsonb_object_keys(before_row||after_row) key) k WHERE before_row->key IS DISTINCT FROM after_row->key;
 IF changed IS NULL THEN RETURN NULL; END IF;
 entity:=coalesce(after_row->>'id',before_row->>'id',after_row->>'revision_id',before_row->>'revision_id')::uuid;
 INSERT INTO app.entity_change_events(entity_table,entity_id,change_type,state_before,state_after,changed_fields) VALUES(TG_TABLE_NAME,entity,lower(TG_OP),coalesce(before_row->>'state',before_row->>'status'),coalesce(after_row->>'state',after_row->>'status'),changed);
 RETURN NULL;
END $$;
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['patients','studies','protocol_revisions','protocol_annotations','assets','colposcopy_tasks','result_delivery_jobs','report_template_versions','users','doctors'] LOOP
 EXECUTE format('CREATE TRIGGER audit_entity_change AFTER INSERT OR UPDATE OR DELETE ON app.%I FOR EACH ROW EXECUTE FUNCTION app.log_entity_change()',tbl);
 END LOOP;
END $$;
CREATE TRIGGER immutable_entity_change BEFORE UPDATE OR DELETE ON app.entity_change_events FOR EACH ROW EXECUTE FUNCTION app.deny_audit_rewrite();
GRANT SELECT ON app.activity_actions TO colposcopy_app;
GRANT INSERT,UPDATE ON app.api_request_log TO colposcopy_app;
GRANT SELECT,INSERT ON app.client_activity_events TO colposcopy_app;
REVOKE ALL ON FUNCTION app.audit_workstation_registry(),app.record_workstation_registry(),app.snapshot_audit_actor(),app.deny_audit_rewrite(),app.log_entity_change() FROM PUBLIC;

CREATE FUNCTION app.guard_request_log() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' OR OLD.finished_at IS NOT NULL OR (to_jsonb(NEW)-ARRAY['finished_at','http_status','duration_ms','safe_error_code']) IS DISTINCT FROM (to_jsonb(OLD)-ARRAY['finished_at','http_status','duration_ms','safe_error_code']) THEN RAISE EXCEPTION 'Request history is immutable' USING ERRCODE='55000'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER immutable_request_identity BEFORE UPDATE OR DELETE ON app.api_request_log FOR EACH ROW EXECUTE FUNCTION app.guard_request_log();
REVOKE ALL ON FUNCTION app.guard_request_log() FROM PUBLIC;
ALTER TABLE app.task_inbox ADD COLUMN database_txid bigint,ADD COLUMN request_id uuid;
ALTER TABLE app.task_inbox ALTER COLUMN database_txid SET DEFAULT txid_current();

GRANT SELECT(request_id) ON app.api_request_log TO colposcopy_app;
INSERT INTO app.activity_actions(code,name,source) VALUES
 ('server.patient.created','Создан пациент','server_request'),('server.study.created','Создано обследование','server_request'),
 ('server.protocol.saved','Сохранена редакция протокола','server_request'),('server.protocol.approved','Утвержден протокол','server_request'),
 ('server.study.reopened','Открыта новая редакция','server_request'),('server.asset.uploaded','Снимок принят сервером','server_request'),
 ('server.template.published','Опубликован шаблон','server_request'),('server.report.created','Отчет зарегистрирован','server_request'),
 ('server.study.published','Опубликован результат обследования','server_request'),('server.user.login_1c','Подтвержден вход через 1С','server_request'),
 ('server.user.logout','Выполнен выход','server_request'),('server.offline_grant.issued','Выдан автономный допуск','server_request'),
 ('server.patient.refreshed_from_1c','Обновлена карточка из 1С','server_request'),('server.study.identity_linked','Прием привязан к пациенту 1С','server_request'),
 ('server.task.claimed','Врач взял задание в работу','server_request'),('server.result.delivery.queued','PDF поставлен в очередь доставки','server_request');
CREATE INDEX request_key_time ON app.api_request_log(credential_id,started_at);

INSERT INTO app.activity_actions(code,name,source) VALUES
 ('api.tasks.receive','Прием назначения от 1С','server_request'),('api.protocol.approve','Запрошено утверждение протокола','server_request'),('api.protocol.reopen','Запрошена новая редакция','server_request'),
 ('api.protocol.save','Запрошено сохранение протокола','server_request'),('api.task.claim','Запрошено взять задание в работу','server_request'),('api.patient.refresh','Запрошено обновление пациента из 1С','server_request'),
 ('api.patient.create','Запрошено создание пациента','server_request'),('api.study.create','Запрошено создание обследования','server_request'),('api.image.upload','Передача снимка серверу','server_request'),('api.result.send','Передача PDF для МИС','server_request');
CREATE OR REPLACE VIEW app.activity_history AS
 SELECT a.id AS event_id,a.occurred_at,'server_transaction'::text AS source,a.action AS action_code,
 coalesce(n.name,'Серверное действие: '||a.action) AS action_name,a.organization_id,a.workstation_id,a.credential_id,a.actor_id AS user_id,a.doctor_id,
 a.workstation_name,a.key_name,a.actor_name AS user_name,a.doctor_name,a.entity_id,a.request_id,a.operation_id,a.database_txid,NULL::smallint AS http_status,a.context_quality
 FROM app.audit_events a LEFT JOIN app.activity_actions n ON n.code='server.'||a.action
 UNION ALL
 SELECT event_id,received_at,'client_reported',action_code,action_name,organization_id,workstation_id,credential_id,user_id,doctor_id,workstation_name,key_name,user_name,doctor_name,entity_id,NULL::uuid,operation_id,NULL::bigint,NULL::smallint,'reported_not_verified' FROM app.client_activity_events
 UNION ALL
 SELECT request_id,started_at,'server_request',action_code,action_name,organization_id,workstation_id,credential_id,user_id,doctor_id,workstation_name,key_name,user_name,doctor_name,requested_entity_id,request_id,operation_id,NULL::bigint,http_status,'request_not_business_commit' FROM app.api_request_log
 UNION ALL
 SELECT id,occurred_at,'registry_administration',action,
 CASE action WHEN 'credential.issued' THEN 'Выдан ключ рабочего места' WHEN 'credential.disabled' THEN 'Отключен ключ рабочего места' WHEN 'credential.enabled' THEN 'Включен ключ рабочего места' WHEN 'credential.changed' THEN 'Изменены настройки ключа' WHEN 'workstation.created' THEN 'Создано рабочее место' WHEN 'workstation.disabled' THEN 'Отключено рабочее место' WHEN 'workstation.changed' THEN 'Изменено рабочее место' ELSE 'Начальное состояние реестра' END,
 organization_id,workstation_id,credential_id,NULL::uuid,NULL::uuid,workstation_name,key_name,coalesce(operator_label,sql_actor),NULL::text,coalesce(credential_id,workstation_id),NULL::uuid,NULL::uuid,database_txid,NULL::smallint,'operator_not_physician_session' FROM app.workstation_registry_events
 UNION ALL
 SELECT e.id,e.occurred_at,'database_change','db.'||e.entity_table||'.'||e.change_type,
 (CASE e.entity_table WHEN 'patients' THEN 'Пациент' WHEN 'studies' THEN 'Обследование' WHEN 'protocol_revisions' THEN 'Редакция протокола' WHEN 'protocol_annotations' THEN 'Разметка снимка' WHEN 'assets' THEN 'Снимок или файл' WHEN 'colposcopy_tasks' THEN 'Задание 1С' WHEN 'result_delivery_jobs' THEN 'Доставка PDF в МИС' WHEN 'report_template_versions' THEN 'Шаблон отчета' WHEN 'users' THEN 'Учетная запись' WHEN 'doctors' THEN 'Карточка врача' ELSE e.entity_table END)
 ||' · '||(CASE e.change_type WHEN 'insert' THEN 'создано' WHEN 'update' THEN 'изменено' ELSE 'удалено' END)
 ||CASE WHEN e.state_after IS NOT NULL THEN ' ('||coalesce(e.state_before,'—')||' → '||e.state_after||')' ELSE '' END,
 a.organization_id,a.workstation_id,a.credential_id,a.actor_id,a.doctor_id,a.workstation_name,a.key_name,a.actor_name,a.doctor_name,e.entity_id,a.request_id,a.operation_id,e.database_txid,NULL::smallint,
 CASE WHEN a.id IS NOT NULL THEN 'context_via_transaction' ELSE 'service_or_unknown_actor' END
 FROM app.entity_change_events e LEFT JOIN LATERAL (
  SELECT candidate.* FROM app.audit_events candidate WHERE candidate.database_txid=e.database_txid
   AND NOT EXISTS(SELECT 1 FROM app.audit_events other WHERE other.database_txid=e.database_txid AND ROW(other.actor_id,other.credential_id) IS DISTINCT FROM ROW(candidate.actor_id,candidate.credential_id))
  ORDER BY candidate.occurred_at,candidate.id LIMIT 1
 ) a ON true;
 REVOKE ALL ON app.activity_history FROM PUBLIC;
