//! Locked and unlocked mode (ROLE-001 – ROLE-008).
//!
//! The application starts unlocked. Locking needs no PIN but is only possible once a
//! PIN is set; unlocking needs the PIN. The PIN is stored only as an Argon2 hash.

use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use tracing::{info, warn};

/// Minimum PIN length.
pub const MIN_PIN_LENGTH: usize = 4;
/// After this many wrong PINs in a row, unlocking is refused for `WAIT_AFTER_FAILURES`.
const MAX_FAILURES: u32 = 5;
const WAIT_AFTER_FAILURES: Duration = Duration::from_secs(30);

pub struct Lock {
    /// Argon2 hash of the PIN (PHC string format).
    pin_hash: Option<String>,
    locked: bool,
    failures: u32,
    wait_until: Option<Instant>,
}

/// Why unlocking failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockError {
    WrongPin,
    /// Too many wrong PINs; try again after this long.
    Wait(Duration),
}

impl Lock {
    /// Unlocked (ROLE-001), with the PIN hash from the configuration.
    pub fn new(pin_hash: Option<String>) -> Self {
        let pin_hash = pin_hash.filter(|hash| {
            let valid = PasswordHash::new(hash).is_ok();
            if !valid {
                warn!("invalid PIN hash in the configuration, ignored");
            }
            valid
        });
        Lock {
            pin_hash,
            locked: false,
            failures: 0,
            wait_until: None,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn has_pin(&self) -> bool {
        self.pin_hash.is_some()
    }

    pub fn pin_hash(&self) -> Option<&str> {
        self.pin_hash.as_deref()
    }

    /// Sets a new PIN; only in unlocked mode (ROLE-007).
    pub fn set_pin(&mut self, pin: &str) -> Result<(), String> {
        if self.locked {
            return Err("settings are locked".into());
        }
        check_pin(pin)?;
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(pin.as_bytes(), &salt)
            .map_err(|e| e.to_string())?
            .to_string();
        self.pin_hash = Some(hash);
        info!("PIN set");
        Ok(())
    }

    /// Removes the PIN; only in unlocked mode. Locking is then not possible.
    pub fn remove_pin(&mut self) {
        if !self.locked && self.pin_hash.take().is_some() {
            info!("PIN removed");
        }
    }

    /// "Lock settings" (ROLE-003): no PIN needed, but one must be set (ROLE-007).
    pub fn lock(&mut self) -> bool {
        if self.locked || self.pin_hash.is_none() {
            return self.locked;
        }
        self.locked = true;
        info!("settings locked");
        true
    }

    /// "Unlock settings" with the PIN (ROLE-006).
    pub fn unlock(&mut self, pin: &str) -> Result<(), UnlockError> {
        if !self.locked {
            return Ok(());
        }
        if let Some(until) = self.wait_until {
            let now = Instant::now();
            if now < until {
                return Err(UnlockError::Wait(until - now));
            }
            self.wait_until = None;
        }
        let correct = self.pin_hash.as_deref().is_some_and(|hash| {
            PasswordHash::new(hash).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(pin.as_bytes(), &hash)
                    .is_ok()
            })
        });
        if correct {
            self.locked = false;
            self.failures = 0;
            info!("settings unlocked");
            Ok(())
        } else {
            self.failures += 1;
            warn!(failures = self.failures, "unlock refused: wrong PIN");
            if self.failures >= MAX_FAILURES {
                self.failures = 0;
                self.wait_until = Some(Instant::now() + WAIT_AFTER_FAILURES);
                warn!(
                    wait_s = WAIT_AFTER_FAILURES.as_secs(),
                    "too many wrong PINs, unlocking paused"
                );
                return Err(UnlockError::Wait(WAIT_AFTER_FAILURES));
            }
            Err(UnlockError::WrongPin)
        }
    }
}

/// A PIN must have at least `MIN_PIN_LENGTH` characters and no surrounding spaces.
pub fn check_pin(pin: &str) -> Result<(), String> {
    if pin.chars().count() < MIN_PIN_LENGTH {
        return Err(format!(
            "The PIN needs at least {MIN_PIN_LENGTH} characters."
        ));
    }
    if pin.trim() != pin {
        return Err("The PIN must not start or end with a space.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_unlocked_and_needs_a_pin_to_lock() {
        let mut lock = Lock::new(None);
        assert!(!lock.is_locked());
        assert!(!lock.lock(), "no PIN: locking is not possible");
        assert!(lock.set_pin("12").is_err());
        lock.set_pin("1234").unwrap();
        assert!(lock.pin_hash().unwrap().starts_with("$argon2"));
        assert!(!lock.pin_hash().unwrap().contains("1234"));
        assert!(lock.lock());
        assert!(lock.set_pin("5678").is_err(), "no PIN change while locked");
        lock.remove_pin();
        assert!(lock.has_pin(), "no PIN removal while locked");
    }

    #[test]
    fn unlock_needs_the_right_pin() {
        let mut lock = Lock::new(None);
        lock.set_pin("2468").unwrap();
        // The hash survives a restart; the application starts unlocked.
        let mut lock = Lock::new(lock.pin_hash().map(str::to_owned));
        assert!(!lock.is_locked());
        lock.lock();
        assert_eq!(lock.unlock("1357"), Err(UnlockError::WrongPin));
        assert!(lock.is_locked());
        assert_eq!(lock.unlock(""), Err(UnlockError::WrongPin));
        assert_eq!(lock.unlock("2468"), Ok(()));
        assert!(!lock.is_locked());
    }

    #[test]
    fn repeated_wrong_pins_pause_unlocking() {
        let mut lock = Lock::new(None);
        lock.set_pin("2468").unwrap();
        lock.lock();
        for _ in 1..MAX_FAILURES {
            assert_eq!(lock.unlock("0000"), Err(UnlockError::WrongPin));
        }
        assert!(matches!(lock.unlock("0000"), Err(UnlockError::Wait(_))));
        // Even the right PIN is refused during the wait.
        assert!(matches!(lock.unlock("2468"), Err(UnlockError::Wait(_))));
        assert!(lock.is_locked());
    }

    #[test]
    fn invalid_hash_is_ignored() {
        let lock = Lock::new(Some("not a hash".into()));
        assert!(!lock.has_pin());
    }
}
