-- Guest accounts keep their existing data; access is limited by current account and grant checks.
ALTER TABLE users ADD COLUMN guest INTEGER NOT NULL DEFAULT 0 CHECK(guest IN (0, 1)) CHECK(guest = 0 OR role = 'user');
INSERT OR IGNORE INTO settings (key, value) VALUES ('share_own_groups', '0'), ('guest_max_days', '90');
