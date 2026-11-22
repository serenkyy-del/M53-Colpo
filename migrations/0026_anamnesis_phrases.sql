-- Anamnesis already lives in protocol_revisions.form.history. No clinical rewrite.
INSERT INTO app.phrase_template_fields(field_code,name,target_kind,max_bytes,enabled)
VALUES ('anamnesis','Анамнез','protocol',16384,true);
