use super::*;

#[test]
fn a_challenge_is_taken_back_once_and_for_what_it_was_given_for() {
    let memory = Memory::default();
    let challenge = memory.issue(Scope::Register { user: 7 });
    assert_eq!(challenge.len(), 32);
    // Not one that was never given out
    assert!(memory.take(&[1; 32]).is_none());
    let (bytes, scope) = memory.take(&challenge).unwrap();
    assert_eq!((bytes, scope), (challenge.clone(), Scope::Register { user: 7 }));
    // Used once
    assert!(memory.take(&challenge).is_none());
}

#[test]
fn challenges_are_all_different() {
    let memory = Memory::default();
    let a = memory.issue(Scope::Login);
    let b = memory.issue(Scope::Login);
    assert_ne!(a, b);
}

#[test]
fn an_old_challenge_is_not_taken() {
    let memory = Memory::default();
    let challenge = memory.issue(Scope::Login);
    for c in memory.challenges.lock().values_mut() {
        c.created = Instant::now().checked_sub(CHALLENGE_TTL + Duration::from_secs(1)).unwrap();
    }
    assert!(memory.take(&challenge).is_none());
}

#[test]
fn the_challenges_kept_are_bounded() {
    let memory = Memory::default();
    for _ in 0..MAX_KEPT + 10 {
        memory.issue(Scope::Login);
    }
    assert!(memory.challenges.lock().len() <= MAX_KEPT);
}
