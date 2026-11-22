-- Optional common diagnosis note lives in protocol_revisions.form JSONB.
-- Preserve clinical revisions, per-row legacy notes and immutable receipts.
INSERT INTO app.phrase_template_fields(field_code,name,target_kind,max_bytes,enabled)
VALUES ('diagnosis_note','Дополнение к диагнозам','protocol',16384,true);
