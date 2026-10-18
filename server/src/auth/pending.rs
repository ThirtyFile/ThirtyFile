//! Connections that haven't signed in yet, per address. The protocol listeners (SFTP, FTP) have a fixed number of
//! slots; capping the connections each address may hold before signing in keeps one host from filling them all.

use std::{collections::HashMap, net::IpAddr, sync::Arc};

/// Connections an address may hold open before signing in
pub const PER_ADDRESS: usize = 8;

#[derive(Clone, Default)]
pub struct Pending {
    counts: Arc<crate::sync::Mutex<HashMap<String, usize>>>,
}

impl Pending {
    /// A slot for a new connection from `ip`, or None when the address already has `PER_ADDRESS` waiting (IPv6
    /// addresses count by their /64, as the sign-in limit does)
    pub fn admit(&self, ip: IpAddr) -> Option<PendingGuard> {
        let key = super::limit_key_ip(&ip.to_string());
        let mut counts = self.counts.lock();
        let count = counts.entry(key.clone()).or_default();
        if *count >= PER_ADDRESS {
            return None;
        }
        *count += 1;
        Some(PendingGuard { counts: self.counts.clone(), key, held: true })
    }
}

/// Held while a connection hasn't signed in: released once it does (`signed_in`), or when it ends
pub struct PendingGuard {
    counts: Arc<crate::sync::Mutex<HashMap<String, usize>>>,
    key: String,
    held: bool,
}

impl PendingGuard {
    pub fn signed_in(&mut self) {
        if !std::mem::replace(&mut self.held, false) {
            return;
        }
        let mut counts = self.counts.lock();
        if let Some(count) = counts.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.signed_in();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_holds_a_few_connections_before_signing_in_and_gets_them_back() {
        let pending = Pending::default();
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let mut guards: Vec<_> = (0..PER_ADDRESS).map(|_| pending.admit(ip).unwrap()).collect();
        assert!(pending.admit(ip).is_none(), "the address is full");
        assert!(pending.admit("203.0.113.8".parse().unwrap()).is_some(), "other addresses aren't affected");
        guards[0].signed_in();
        let _again = pending.admit(ip).expect("a connection that signed in frees its slot");
        drop(guards.pop());
        assert!(pending.admit(ip).is_some(), "so does one that ends");
        // IPv6 counts by /64
        let v6: Vec<_> = (0..PER_ADDRESS).map(|i| pending.admit(format!("2001:db8::{}", i + 1).parse().unwrap()).unwrap()).collect();
        assert!(pending.admit("2001:db8::ffff".parse().unwrap()).is_none());
        drop(v6);
    }
}
