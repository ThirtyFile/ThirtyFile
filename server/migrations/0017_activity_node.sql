-- An item's history in the Details pane looks up activity by item. Like the other log indexes (0008), the column alone:
-- the index ends in the row id, so the newest entries come first without sorting.
CREATE INDEX activity_node ON activity (node_id);
