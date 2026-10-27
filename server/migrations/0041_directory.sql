-- Directory sign-in (directory/): groups that follow a directory group are the directory's ('ldap'), and an account the
-- directory turned off is told apart from one an administrator turned off, so it is turned on again only when the person
-- is back in the directory
ALTER TABLE groups ADD COLUMN source TEXT NOT NULL DEFAULT 'local';
ALTER TABLE users ADD COLUMN directory_disabled INTEGER NOT NULL DEFAULT 0;
