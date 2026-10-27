//! What passkeys keep in memory: the challenges given out. Each is random, used once, and valid for two minutes.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{sync::Mutex, util::sha256_hex};

const CHALLENGE_TTL: Duration = Duration::from_secs(120);
/// Challenges kept at most: the oldest go first
const MAX_KEPT: usize = 5000;

/// What a challenge was given for: an answer is only taken for the kind of ceremony it was issued for
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Signing in with a passkey, no user name typed
    Login,
    /// Adding a passkey to the signed-in account
    Register { user: i64 },
    /// The second step of a sign-in by password (the SHA-256 of its ticket)
    Second { ticket: String },
}

struct Challenge {
    bytes: Vec<u8>,
    scope: Scope,
    created: Instant,
}

#[derive(Default)]
pub struct Memory {
    challenges: Mutex<HashMap<String, Challenge>>,
}

impl Memory {
    /// A new challenge (32 random bytes) for `scope`
    pub fn issue(&self, scope: Scope) -> Vec<u8> {
        let bytes = rand::random::<[u8; 32]>().to_vec();
        let mut map = self.challenges.lock();
        map.retain(|_, c| c.created.elapsed() < CHALLENGE_TTL);
        while map.len() >= MAX_KEPT {
            let Some(oldest) = map.iter().min_by_key(|(_, c)| c.created).map(|(k, _)| k.clone()) else { break };
            map.remove(&oldest);
        }
        map.insert(sha256_hex(&bytes), Challenge { bytes: bytes.clone(), scope, created: Instant::now() });
        bytes
    }

    /// Takes the challenge back, once: what it was given for, when it is one of ours and still valid
    pub fn take(&self, challenge: &[u8]) -> Option<(Vec<u8>, Scope)> {
        let found = self.challenges.lock().remove(&sha256_hex(challenge))?;
        (found.created.elapsed() < CHALLENGE_TTL).then_some((found.bytes, found.scope))
    }
}

#[cfg(test)]
mod tests;
