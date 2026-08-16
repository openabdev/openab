//! Transient-secret containment for the runtime process.
//!
//! `PR_SET_DUMPABLE=0` is deliberately applied before this process accepts any
//! credential or mints a token.  File permissions do not isolate same-UID
//! processes; on Linux, this setting is the barrier that prevents a managed
//! shell from reading the runtime through `/proc/<pid>/mem` or inherited FDs.

use crate::Error;
use std::sync::atomic::{compiler_fence, Ordering};

/// An owned plaintext buffer which overwrites its storage on demand and on drop.
///
/// Callers pass this type, rather than `String`, across authentication boundaries
/// so verification can erase the presented bearer value before returning.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Erase plaintext promptly once it is no longer needed.
    pub fn zeroize(&mut self) {
        // Volatile writes make the erase observable to the compiler even when
        // the buffer is immediately dropped afterwards.
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(value: Vec<u8>) -> Self {
        Self::new(value)
    }
}

/// Result of applying the Linux-only transient-secret barrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentStatus {
    /// Linux `PR_SET_DUMPABLE=0` succeeded and was read back successfully.
    Active,
    /// Non-Linux targets intentionally perform no containment action. They are
    /// out of scope for the MVP and must not be represented as protected.
    Unsupported,
}

/// Apply the transient-window barrier before accepting secret material.
///
/// On Linux, failure is fail-closed: callers must not read a credential or
/// serve requests after an error. On non-Linux this is an explicit no-op and
/// returns [`ContainmentStatus::Unsupported`], never a misleading success.
pub fn establish_transient_secret_barrier() -> Result<ContainmentStatus, Error> {
    #[cfg(target_os = "linux")]
    {
        set_dumpable_zero()?;
        assert_dumpable_zero()?;
        Ok(ContainmentStatus::Active)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(ContainmentStatus::Unsupported)
    }
}

/// Regression guard for startup and Linux adversary tests.
///
/// A dependency that accidentally sets dumpability back to one invalidates the
/// same-UID containment model; make that a direct, testable failure.
pub fn assert_dumpable_zero() -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        let value = unsafe { libc::prctl(libc::PR_GET_DUMPABLE) };
        if value == 0 {
            Ok(())
        } else if value < 0 {
            Err(Error::Io(std::io::Error::last_os_error()))
        } else {
            Err(Error::Other(format!(
                "PR_GET_DUMPABLE returned {value}; refusing to handle transient secrets"
            )))
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::Other(
            "dumpability containment is Linux-only; non-Linux MVP runtime is unsupported".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn set_dumpable_zero() -> Result<(), Error> {
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bytes_zeroize_explicitly() {
        let mut secret = SecretBytes::from_slice(b"synthetic-test-secret");
        secret.zeroize();
        assert!(secret.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_barrier_is_set_and_read_back() {
        let original = unsafe { libc::prctl(libc::PR_GET_DUMPABLE) };
        assert!(original >= 0, "PR_GET_DUMPABLE must be available for this test");
        establish_transient_secret_barrier().expect("PR_SET_DUMPABLE=0 must succeed");
        assert_dumpable_zero().expect("the regression guard must observe dumpable=0");
        let restore = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, original) };
        assert_eq!(restore, 0, "restore test process dumpability");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_does_not_claim_containment() {
        assert_eq!(
            establish_transient_secret_barrier().unwrap(),
            ContainmentStatus::Unsupported
        );
        assert!(assert_dumpable_zero().is_err());
    }
}
