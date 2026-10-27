//! A small LDAP client (RFC 4511): what a directory sign-in needs, written here as ThirtyFTP and ThirtyS3 were, so that
//! no library of the size of a directory server's own comes with it. `ber` is the encoding, `filter` the search
//! filters (the user name is only ever written into one escaped), `client` the connection.

pub mod ber;
mod client;
pub mod filter;

pub use client::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testserver;
