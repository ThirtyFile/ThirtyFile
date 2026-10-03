-- The interface style a person chose: 'auto' (decided in the browser by the operating system), 'windows' or 'mac'.
-- Kept with the account so it follows them to every device.
ALTER TABLE users ADD COLUMN ui_style TEXT NOT NULL DEFAULT 'auto' CHECK (ui_style IN ('auto', 'windows', 'mac'));
