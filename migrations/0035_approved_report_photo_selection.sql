-- NULL means the approving client did not supply a choice. An empty array is explicit.
-- The existing immutable_revision_identity_snapshot trigger protects the selection.
ALTER TABLE app.revision_identity_snapshots ADD COLUMN selected_photo_ids uuid[]
 CHECK (selected_photo_ids IS NULL OR (
   cardinality(selected_photo_ids) <= 1024 AND array_position(selected_photo_ids,NULL::uuid) IS NULL
 ));
