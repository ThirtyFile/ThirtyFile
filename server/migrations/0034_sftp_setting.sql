-- SFTP's two switches become one JSON setting, as S3's and FTP's are
INSERT INTO settings (key, value)
SELECT 'sftp', json_object(
    'enabled', json(CASE WHEN EXISTS(SELECT 1 FROM settings WHERE key = 'sftp_enabled' AND value = '1') THEN 'true' ELSE 'false' END),
    'keys_enabled', json(CASE WHEN EXISTS(SELECT 1 FROM settings WHERE key = 'sftp_keys_enabled' AND value = '1') THEN 'true' ELSE 'false' END))
WHERE EXISTS(SELECT 1 FROM settings WHERE key IN ('sftp_enabled', 'sftp_keys_enabled'))
ON CONFLICT (key) DO NOTHING;
DELETE FROM settings WHERE key IN ('sftp_enabled', 'sftp_keys_enabled');
