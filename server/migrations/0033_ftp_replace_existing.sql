-- Whether a device's upload replaces a file of the same name in its receive folder (earlier versions are kept, as
-- before), or is kept beside it with a number. Receive folders made before keep replacing.
ALTER TABLE ftp_receive_folders ADD COLUMN replace_existing INTEGER NOT NULL DEFAULT 1 CHECK (replace_existing IN (0, 1));
