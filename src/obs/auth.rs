//! Answering obs-websocket's authentication challenge.
//!
//! OBS never receives the password. Instead it sends two values — a `salt`
//! fixed per password and a `challenge` fresh for every connection — and
//! expects a hash derived from all three:
//!
//! ```text
//! secret         = base64( sha256( password + salt ) )
//! authentication = base64( sha256( secret   + challenge ) )
//! ```
//!
//! The salt means two people who chose the same password produce different
//! secrets, so a stolen `secret` from one machine is useless on another. The
//! per-connection challenge means the value sent over the wire is different
//! every time, so capturing one does not let it be replayed.
//!
//! This is exactly the scheme obs-websocket 5 specifies; it is reproduced
//! rather than invented, and the test below pins it against the published
//! example vector.

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Compute the answer to a challenge.
///
/// The password is never stored, logged, or returned by this function — only
/// the hash derived from it goes anywhere.
pub fn compute(password: &str, salt: &str, challenge: &str) -> String {
    let secret = {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        hasher.update(salt.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
    };

    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(challenge.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from the obs-websocket 5 specification. If this ever
    /// fails, the implementation has drifted from the protocol and no OBS
    /// with a password set will accept a connection.
    #[test]
    fn the_published_example_vector_matches() {
        let result = compute(
            "supersecretpassword",
            "PZVbYpvAnZut2SS3k3tnTQ==",
            "lfYW3AhFLp2YcILmwSQ9rSFRIiEQgxuEk5hSyQ3XGaQ=",
        );
        assert_eq!(result, "KyqYIxIYmV+kMWMia3ahAvmhvF16ReqnQK6KLN9onU4=");
    }

    #[test]
    fn the_same_inputs_always_give_the_same_answer() {
        assert_eq!(compute("p", "s", "c"), compute("p", "s", "c"));
    }

    /// Each of the three inputs has to affect the result, or one of them is
    /// not doing its job.
    #[test]
    fn every_input_changes_the_answer() {
        let base = compute("password", "salt", "challenge");
        assert_ne!(base, compute("different", "salt", "challenge"));
        assert_ne!(base, compute("password", "different", "challenge"));
        assert_ne!(base, compute("password", "salt", "different"));
    }

    /// The answer must not contain the password, in any form. This is a
    /// cheap check of an important property: this value is sent over a
    /// network connection that is usually unencrypted.
    #[test]
    fn the_answer_does_not_contain_the_password() {
        let password = "supersecretpassword";
        let answer = compute(password, "PZVbYpvAnZut2SS3k3tnTQ==", "challenge");
        assert!(!answer.contains(password));
        assert!(!answer.contains("supersecret"));
    }
}
