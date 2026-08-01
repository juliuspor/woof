use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::ApiToken;

pub const HEALTH_CHALLENGE_HEADER: &str = "x-woof-health-challenge";
pub const HEALTH_PROOF_HEADER: &str = "x-woof-health-proof";
pub const HEALTH_CHALLENGE_BYTES: usize = 32;

/// Creates a one-use public challenge for authenticating the fixed loopback
/// service without transmitting its bearer token.
pub fn generate_health_challenge() -> String {
    let mut bytes = [0_u8; HEALTH_CHALLENGE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Computes the response for a canonical lowercase-hex health challenge.
pub fn health_proof(token: &ApiToken, challenge: &str) -> Option<String> {
    let challenge_bytes = parse_challenge(challenge)?;
    Some(hex::encode(hmac_sha256(token.expose(), &challenge_bytes)))
}

/// Verifies a health response in constant time after strict shape validation.
pub fn verify_health_proof(token: &ApiToken, challenge: &str, candidate: &str) -> bool {
    let Some(expected) = health_proof(token, challenge) else {
        return false;
    };
    candidate.len() == expected.len() && candidate.as_bytes().ct_eq(expected.as_bytes()).into()
}

fn parse_challenge(challenge: &str) -> Option<[u8; HEALTH_CHALLENGE_BYTES]> {
    if challenge.len() != HEALTH_CHALLENGE_BYTES * 2
        || !challenge
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return None;
    }
    let decoded = hex::decode(challenge).ok()?;
    decoded.try_into().ok()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut key_block = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges_are_random_canonical_and_verifiable() {
        let token = ApiToken::generate();
        let first = generate_health_challenge();
        let second = generate_health_challenge();
        assert_ne!(first, second);
        assert_eq!(first.len(), HEALTH_CHALLENGE_BYTES * 2);

        let proof = health_proof(&token, &first).unwrap();
        assert!(verify_health_proof(&token, &first, &proof));
        assert!(!verify_health_proof(&token, &second, &proof));
        assert!(!verify_health_proof(&token, &first, "short"));
    }

    #[test]
    fn malformed_challenges_are_rejected() {
        let token = ApiToken::generate();
        assert!(health_proof(&token, "short").is_none());
        assert!(health_proof(&token, &"A".repeat(64)).is_none());
        assert!(health_proof(&token, &"z".repeat(64)).is_none());
    }
}
