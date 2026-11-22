-- Text phrase libraries are separate from printed report templates.
ALTER TABLE app.users ADD CONSTRAINT users_identity_org UNIQUE(id,organization_id);
CREATE TABLE app.phrase_template_fields (
 field_code text PRIMARY KEY,name text NOT NULL,target_kind text NOT NULL CHECK(target_kind IN ('protocol','mark')),
 max_bytes integer NOT NULL CHECK(max_bytes BETWEEN 1 AND 16384),enabled boolean NOT NULL DEFAULT true
);
INSERT INTO app.phrase_template_fields(field_code,name,target_kind,max_bytes) VALUES
('conclusion','Заключение врача','protocol',16384),
('extended.clock','Соответствие с циферблатом','protocol',16384),
('extended.other','Прочие данные расширенной кольпоскопии','protocol',16384),
('rectovaginal.description','Ректо-вагинальный осмотр','protocol',16384),
('vulva.other_comment','Вульва: различные находки','protocol',16384),
('vulva.cancer_comment','Вульва: подозрение на рак','protocol',16384),
('cervix.adequacy_reason','Причина неадекватности оценки шейки','protocol',16384),
('vulva.adequacy_reason','Причина неадекватности оценки вульвы','protocol',16384),
('anus.adequacy_reason','Причина неадекватности оценки анальной области','protocol',16384),
('vulva.other.trauma.comment','Травма','mark',16384),
('vulva.other.malformation.comment','Порок развития','mark',16384),
('vulva.other.hematoma.comment','Гематома','mark',16384),
('vulva.other.hemangioma.comment','Гемангиома','mark',16384),
('vulva.cancer.neoplasia.comment','Явная неоплазия','mark',16384),
('vulva.cancer.ulceration.comment','Изъязвление','mark',16384),
('vulva.cancer.necrosis.comment','Некроз','mark',16384),
('vulva.cancer.bleeding.comment','Кровоточивость','mark',16384),
('vulva.cancer.hyperkeratosis.comment','Гиперкератоз','mark',16384),
('vulva.cancer.exophytic.comment','Экзофит','mark',16384);
CREATE TABLE app.phrase_templates (
 id uuid PRIMARY KEY CHECK(id<>'00000000-0000-0000-0000-000000000000'),organization_id uuid NOT NULL REFERENCES app.organizations,
 field_code text NOT NULL REFERENCES app.phrase_template_fields,scope text NOT NULL CHECK(scope IN ('personal','shared')),
 owner_user_id uuid,title text NOT NULL CHECK(char_length(title) BETWEEN 1 AND 200 AND btrim(title)<>''),
 body text NOT NULL CHECK(octet_length(body) BETWEEN 1 AND 16384 AND btrim(body)<>''),
 row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0),created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),created_by uuid NOT NULL,updated_by uuid NOT NULL,
 CHECK((scope='personal')=(owner_user_id IS NOT NULL)),
 FOREIGN KEY(owner_user_id,organization_id) REFERENCES app.users(id,organization_id),
 FOREIGN KEY(created_by,organization_id) REFERENCES app.users(id,organization_id),
 FOREIGN KEY(updated_by,organization_id) REFERENCES app.users(id,organization_id)
);
CREATE INDEX phrase_library_lookup ON app.phrase_templates(organization_id,field_code,scope,owner_user_id,id);
CREATE TABLE app.user_phrase_preferences (
 organization_id uuid NOT NULL,user_id uuid PRIMARY KEY,apply_mode text NOT NULL CHECK(apply_mode IN ('append','replace')),
 include_shared boolean NOT NULL,row_version bigint NOT NULL DEFAULT 1 CHECK(row_version>0),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(user_id,organization_id) REFERENCES app.users(id,organization_id)
);
CREATE FUNCTION app.guard_phrase_template() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
DECLARE limit_bytes integer;
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'Phrase templates are retained' USING ERRCODE='55000'; END IF;
 SELECT max_bytes INTO limit_bytes FROM app.phrase_template_fields WHERE field_code=NEW.field_code AND enabled;
 IF limit_bytes IS NULL OR octet_length(NEW.body)>limit_bytes THEN RAISE EXCEPTION 'Invalid phrase field or size' USING ERRCODE='23514'; END IF;
 IF TG_OP='UPDATE' THEN
  IF ROW(NEW.id,NEW.organization_id,NEW.field_code,NEW.scope,NEW.owner_user_id,NEW.created_by,NEW.created_at) IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.field_code,OLD.scope,OLD.owner_user_id,OLD.created_by,OLD.created_at) THEN RAISE EXCEPTION 'Phrase identity is immutable' USING ERRCODE='55000'; END IF;
  NEW.row_version:=OLD.row_version+1;
 ELSE NEW.row_version:=1;
 END IF;
 NEW.updated_at:=clock_timestamp(); RETURN NEW;
END $$;
CREATE TRIGGER phrase_guard BEFORE INSERT OR UPDATE OR DELETE ON app.phrase_templates FOR EACH ROW EXECUTE FUNCTION app.guard_phrase_template();
CREATE FUNCTION app.guard_phrase_preferences() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,app AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'Phrase preferences are retained' USING ERRCODE='55000'; END IF;
 IF TG_OP='UPDATE' THEN
  IF ROW(NEW.user_id,NEW.organization_id) IS DISTINCT FROM ROW(OLD.user_id,OLD.organization_id) THEN RAISE EXCEPTION 'Preference identity is immutable' USING ERRCODE='55000'; END IF;
  NEW.row_version:=OLD.row_version+1;
 ELSE NEW.row_version:=1;
 END IF;
 NEW.updated_at:=clock_timestamp(); RETURN NEW;
END $$;
CREATE TRIGGER phrase_preferences_guard BEFORE INSERT OR UPDATE OR DELETE ON app.user_phrase_preferences FOR EACH ROW EXECUTE FUNCTION app.guard_phrase_preferences();
CREATE TRIGGER audit_entity_change AFTER INSERT OR UPDATE ON app.phrase_templates FOR EACH ROW EXECUTE FUNCTION app.log_entity_change();
-- Preferences have user_id, not id; transaction audit records their entity/version explicitly.
GRANT SELECT ON app.phrase_template_fields TO colposcopy_app;
GRANT SELECT,INSERT,UPDATE ON app.phrase_templates,app.user_phrase_preferences TO colposcopy_app;
REVOKE ALL ON FUNCTION app.guard_phrase_template(),app.guard_phrase_preferences() FROM PUBLIC;
ALTER TABLE app.client_activity_events ADD COLUMN phrase_template_id uuid,
 ADD COLUMN phrase_template_version bigint CHECK(phrase_template_version>0),ADD COLUMN apply_mode text CHECK(apply_mode IN ('append','replace'));
INSERT INTO app.activity_actions(code,name,source) VALUES
 ('client.phrase.applied','Применена фраза к полю','client'),
 ('client.phrase.local_applied','Применена локальная несинхронизированная фраза','client'),
 ('client.phrase.editor_opened','Открыт редактор фразы','client'),
 ('client.phrase.save_requested','Запрошено сохранение фразы','client'),
 ('client.phrase.preferences_changed','Изменены настройки фраз на клиенте','client'),
 ('server.phrase.created','Создан шаблон фразы','server_request'),
 ('server.phrase.updated','Изменен шаблон фразы','server_request'),
 ('server.phrase.preferences_updated','Сохранены настройки фраз','server_request'),
 ('api.phrases.read','Чтение библиотеки фраз','server_request'),
 ('api.phrases.write','Изменение библиотеки фраз','server_request');
