-- Which folders have folders in them, for the folder listings of the navigation pane (nodes.rs, list_children): a
-- folder of many files and no folders is answered without reading its files.
CREATE INDEX nodes_subfolders ON nodes (parent_id) WHERE kind = 'folder' AND trashed_at IS NULL;
