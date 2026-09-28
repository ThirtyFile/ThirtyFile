-- Who moved an item to the trash, shown in the trash's Deleted by column. Set on the trash root only (the item the
-- person deleted); NULL for items deleted before this column existed, which can't be filled in afterwards.
-- No foreign key: deleting a user clears it instead (admin.rs), like other references to deleted users.
ALTER TABLE nodes ADD COLUMN trashed_by INTEGER;
