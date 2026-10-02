-- Whose personal space each activity log entry is about, so the log leaves out, for administrators, the names in
-- entries about someone else's personal space (logs/query.rs), also after the space is gone. private_to: the owner of
-- the personal space; NULL for other spaces and entries about no space; 0 when the space is gone (treated as private).
-- Filled when an entry is recorded; entries recorded before this have it from the spaces still there.
ALTER TABLE activity ADD COLUMN private_to INTEGER;

UPDATE activity SET private_to = (SELECT CASE WHEN d.id IS NULL THEN 0 WHEN d.kind = 'personal' THEN d.owner_id END
                                  FROM (SELECT activity.drive_id AS drive_id) x LEFT JOIN drives d ON d.id = x.drive_id)
WHERE drive_id IS NOT NULL;

CREATE TRIGGER activity_private_to AFTER INSERT ON activity WHEN NEW.drive_id IS NOT NULL BEGIN
  UPDATE activity SET private_to = (SELECT CASE WHEN d.id IS NULL THEN 0 WHEN d.kind = 'personal' THEN d.owner_id END
                                    FROM (SELECT NEW.drive_id AS drive_id) x LEFT JOIN drives d ON d.id = x.drive_id)
  WHERE id = NEW.id;
END;
