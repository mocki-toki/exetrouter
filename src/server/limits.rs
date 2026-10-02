use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const GLOBAL_LIMIT: usize = 64;

pub(super) struct Slots {
    global: Arc<Semaphore>,
    users: Mutex<HashMap<i64, Weak<Semaphore>>>,
    per_user: usize,
}

pub(super) struct Permit {
    _user: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

#[derive(Debug, PartialEq)]
pub(super) enum Full {
    User,
    Global,
}

impl Full {
    pub fn code(&self) -> &'static str {
        match self {
            Self::User => "user_concurrency_limit",
            Self::Global => "server_busy",
        }
    }
    pub fn message(&self) -> &'static str {
        match self {
            Self::User => "user concurrency limit reached; wait for active work to finish",
            Self::Global => "server concurrency limit reached",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub global_limit: usize,
    pub per_user_limit: usize,
    pub active_for_user: usize,
}

impl Slots {
    pub fn new(per_user: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(GLOBAL_LIMIT)),
            users: Mutex::new(HashMap::new()),
            per_user,
        }
    }

    pub fn acquire(&self, user_id: i64) -> std::result::Result<Permit, Full> {
        let user = {
            let mut users = self.users.lock().expect("slot mutex");
            // Only active users retain semaphores. Clean weak entries so idle
            // users cannot accumulate an unbounded registry over time.
            users.retain(|_, value| value.strong_count() > 0);
            let semaphore = users
                .get(&user_id)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let semaphore = Arc::new(Semaphore::new(self.per_user));
                    users.insert(user_id, Arc::downgrade(&semaphore));
                    semaphore
                });
            semaphore.try_acquire_owned().map_err(|_| Full::User)?
        };
        let global = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| Full::Global)?;
        Ok(Permit {
            _user: user,
            _global: global,
        })
    }

    pub fn snapshot(&self, user_id: i64) -> Snapshot {
        let users = self.users.lock().expect("slot mutex");
        let active = users
            .get(&user_id)
            .and_then(Weak::upgrade)
            .map_or(0, |slots| self.per_user - slots.available_permits());
        Snapshot {
            global_limit: GLOBAL_LIMIT,
            per_user_limit: self.per_user,
            active_for_user: active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_and_global_rejection_do_not_strand_user_slots() {
        let slots = Slots::new(1);
        let mut permits = (0..GLOBAL_LIMIT as i64)
            .map(|id| slots.acquire(id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(slots.acquire(0).err(), Some(Full::User));
        assert_eq!(slots.acquire(100).err(), Some(Full::Global));
        assert_eq!(slots.snapshot(100).active_for_user, 0);
        permits.pop();
        let new_user = slots.acquire(100).unwrap();
        assert_eq!(slots.snapshot(100).active_for_user, 1);
        drop(new_user);
        drop(permits);
        for id in 0..1024 {
            drop(slots.acquire(id).unwrap());
        }
        assert!(slots.users.lock().unwrap().len() <= 1);
    }
}
