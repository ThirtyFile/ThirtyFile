-- Whose personal space each share link visit was in, so the visit log can leave out, for administrators, the names
-- and addresses of links in someone else's personal space (logs/query.rs), also after the item and the link are gone.
-- private_to: the owner of the personal space; NULL in other spaces; 0 when the space isn't known (treated as private).
-- Filled when a visit is recorded, from the item visited or else the link's item; visits recorded before this have it
-- from what is still there.
ALTER TABLE share_access ADD COLUMN private_to INTEGER;

UPDATE share_access SET private_to = (
  SELECT CASE WHEN d.id IS NULL THEN 0 WHEN d.kind = 'personal' THEN d.owner_id END
  FROM (SELECT COALESCE((SELECT drive_id FROM nodes WHERE id = share_access.node_id),
                        (SELECT n.drive_id FROM shares s JOIN nodes n ON n.id = s.node_id WHERE s.id = share_access.share_id)) AS drive_id) x
  LEFT JOIN drives d ON d.id = x.drive_id
);

CREATE TRIGGER share_access_private_to AFTER INSERT ON share_access BEGIN
  UPDATE share_access SET private_to = (
    SELECT CASE WHEN d.id IS NULL THEN 0 WHEN d.kind = 'personal' THEN d.owner_id END
    FROM (SELECT COALESCE((SELECT drive_id FROM nodes WHERE id = NEW.node_id),
                          (SELECT n.drive_id FROM shares s JOIN nodes n ON n.id = s.node_id WHERE s.id = NEW.share_id)) AS drive_id) x
    LEFT JOIN drives d ON d.id = x.drive_id
  ) WHERE id = NEW.id;
END;
