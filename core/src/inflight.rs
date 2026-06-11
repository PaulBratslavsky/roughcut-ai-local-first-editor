//! One in-flight guard for "at most one of these per key" background work.
//! RAII: the claim releases on drop, INCLUDING on panic — the hand-rolled
//! sets this replaces leaked their key if the work panicked, wedging that
//! project's background pass until restart.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::Mutex;

pub struct InFlight<K: Eq + Hash + Clone + Send + 'static> {
    set: &'static Mutex<HashSet<K>>,
    key: K,
}

impl<K: Eq + Hash + Clone + Send + 'static> Drop for InFlight<K> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.key);
    }
}

/// Claim `key` in `set`. None = someone else holds it (skip the work).
pub fn try_claim<K: Eq + Hash + Clone + Send + 'static>(
    set: &'static Mutex<HashSet<K>>,
    key: K,
) -> Option<InFlight<K>> {
    if set.lock().unwrap().insert(key.clone()) {
        Some(InFlight { set, key })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn set() -> &'static Mutex<HashSet<u32>> {
        static S: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
        S.get_or_init(Default::default)
    }

    #[test]
    fn claims_release_on_drop_and_panic() {
        let a = try_claim(set(), 1).expect("first claim");
        assert!(try_claim(set(), 1).is_none(), "second claim must fail");
        drop(a);
        assert!(try_claim(set(), 1).is_some(), "released on drop");

        let _ = std::panic::catch_unwind(|| {
            let _guard = try_claim(set(), 2).expect("claim inside panic");
            panic!("boom");
        });
        assert!(try_claim(set(), 2).is_some(), "released even on panic");
    }
}
