//! Passkeys (Web Authentication): sign in with a device's own key instead of, or beside, a password. The reading of what
//! a passkey sends is our own (`cbor`, `cose`, `webauthn`), with `aws-lc-rs`, which the program already links for TLS,
//! checking the signatures: no OpenSSL.

pub mod api;
pub mod cbor;
pub mod cose;
pub mod rp;
pub mod state;
pub mod store;
pub mod webauthn;

#[cfg(test)]
mod testkit;
