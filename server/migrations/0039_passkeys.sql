-- Passkeys (passkeys/): a device's own key that signs a person in, instead of a password or beside it
CREATE TABLE passkeys (
  id              INTEGER PRIMARY KEY,
  user_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  -- What the device names the key by: found when someone signs in without typing a user name
  credential_id   BLOB NOT NULL UNIQUE,
  name            TEXT NOT NULL,
  -- The key, as COSE (RFC 9052)
  public_key      BLOB NOT NULL,
  -- The device's count of sign-ins: one that goes back is refused (a copy of the key); 0 throughout is a passkey that doesn't count
  sign_count      INTEGER NOT NULL DEFAULT 0,
  -- JSON: how the device can be reached (usb, nfc, ble, internal, hybrid)
  transports      TEXT NOT NULL DEFAULT '[]',
  -- Synced between devices (a phone's keychain, a password manager), and whether it is now
  backup_eligible INTEGER NOT NULL DEFAULT 0,
  backup_state    INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  last_used_at    INTEGER
);
CREATE INDEX passkeys_user ON passkeys (user_id);
