//! Proving a UDP address is a SpacetimeDB identity's player.
//!
//! A player's SpacetimeDB identity is what the database trusts, and UDP has no
//! connection to carry it. So the identity vouches for a key pair instead: the
//! player makes an Ed25519 key pair and, over their authenticated SpacetimeDB
//! connection, calls the match module's `join` with the **public** key, which
//! the module stores on the player's seat (a public table, nothing secret in
//! it). Over UDP the player then signs a one-time challenge from the gateway
//! with the private key, and the gateway checks the signature against the
//! public key in the seat. Nobody but the identity's owner can have put that
//! key there, and nobody else can sign.
//!
//! The challenge is a *cookie* the gateway makes without remembering it:
//! `MAC(gateway secret, source address, player, stamp)`. So a Hello from a
//! forged source address gets its challenge sent to the forged owner and no
//! state is kept, a proof made for one address is worthless from another, and
//! `stamp` (strictly increasing, microseconds) lets the gateway refuse a proof
//! it has already accepted. The exchange is in [`crate::datagram`].

use ed25519_compact::{sha512, KeyPair, PublicKey, Seed, Signature};

pub const COOKIE_SIZE: usize = 16;
pub const PUBLIC_KEY_SIZE: usize = 32;
pub const SEED_SIZE: usize = 32;
pub const SIGNATURE_SIZE: usize = 64;

/// How long, in microseconds, a challenge may be answered after it was made.
pub const CHALLENGE_LIFETIME_US: u64 = 30_000_000;

/// What a challenge is made of and what the player signs.
const DOMAIN: &[u8] = b"halo-udp-auth-v1";

/// The cookie of a challenge: bound to the address it is sent to, the player
/// it is for and when it was made, and unforgeable without `secret`.
pub fn cookie(secret: &[u8; 32], addr: &[u8], player: u16, stamp: u64) -> [u8; COOKIE_SIZE] {
    let mut h = sha512::Hash::new();
    h.update(DOMAIN);
    h.update(b"cookie");
    h.update(secret);
    h.update((addr.len() as u8).to_le_bytes());
    h.update(addr);
    h.update(player.to_le_bytes());
    h.update(stamp.to_le_bytes());
    let full = h.finalize();
    let mut out = [0u8; COOKIE_SIZE];
    out.copy_from_slice(&full[..COOKIE_SIZE]);
    out
}

/// Whether two cookies are equal, taking the same time whatever they hold.
pub fn cookie_eq(a: &[u8; COOKIE_SIZE], b: &[u8; COOKIE_SIZE]) -> bool {
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// The bytes a player signs to answer a challenge.
pub fn auth_message(player: u16, stamp: u64, cookie: &[u8; COOKIE_SIZE]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DOMAIN.len() + 2 + 8 + COOKIE_SIZE);
    m.extend_from_slice(DOMAIN);
    m.extend_from_slice(b"proof");
    m.extend_from_slice(&player.to_le_bytes());
    m.extend_from_slice(&stamp.to_le_bytes());
    m.extend_from_slice(cookie);
    m
}

/// The public key of the key pair a 32-byte secret seed makes. Generate the
/// seed from the operating system's random source, keep it, and give the
/// public key to `join`.
pub fn public_key(seed: &[u8; SEED_SIZE]) -> [u8; PUBLIC_KEY_SIZE] {
    *KeyPair::from_seed(Seed::new(*seed)).pk
}

/// Sign the answer to a challenge.
pub fn sign_challenge(
    seed: &[u8; SEED_SIZE],
    player: u16,
    stamp: u64,
    cookie: &[u8; COOKIE_SIZE],
) -> [u8; SIGNATURE_SIZE] {
    let pair = KeyPair::from_seed(Seed::new(*seed));
    *pair.sk.sign(auth_message(player, stamp, cookie), None)
}

/// Whether `signature` answers the challenge with the private key of `public_key`.
pub fn verify_challenge(
    public_key: &[u8],
    player: u16,
    stamp: u64,
    cookie: &[u8; COOKIE_SIZE],
    signature: &[u8; SIGNATURE_SIZE],
) -> bool {
    let (Ok(pk), sig) = (PublicKey::from_slice(public_key), Signature::new(*signature)) else { return false };
    pk.verify(auth_message(player, stamp, cookie), &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7; 32];
    const ADDR: &[u8] = &[127, 0, 0, 1, 0x39, 0x30];

    #[test]
    fn a_signed_challenge_verifies_only_for_its_key_player_stamp_and_cookie() {
        let seed = [1u8; 32];
        let pk = public_key(&seed);
        let c = cookie(&SECRET, ADDR, 5, 1000);
        let sig = sign_challenge(&seed, 5, 1000, &c);
        assert!(verify_challenge(&pk, 5, 1000, &c, &sig));
        assert!(!verify_challenge(&public_key(&[2; 32]), 5, 1000, &c, &sig), "another key");
        assert!(!verify_challenge(&pk, 6, 1000, &c, &sig), "another player");
        assert!(!verify_challenge(&pk, 5, 1001, &c, &sig), "another stamp");
        assert!(!verify_challenge(&pk, 5, 1000, &cookie(&SECRET, ADDR, 5, 1001), &sig), "another cookie");
        assert!(!verify_challenge(&pk[..31], 5, 1000, &c, &sig), "a key that is not one");
    }

    #[test]
    fn a_cookie_is_bound_to_the_secret_address_player_and_stamp() {
        let c = cookie(&SECRET, ADDR, 5, 1000);
        assert_eq!(c, cookie(&SECRET, ADDR, 5, 1000));
        assert_ne!(c, cookie(&[8; 32], ADDR, 5, 1000));
        assert_ne!(c, cookie(&SECRET, &[127, 0, 0, 1, 0x3a, 0x30], 5, 1000));
        assert_ne!(c, cookie(&SECRET, ADDR, 6, 1000));
        assert_ne!(c, cookie(&SECRET, ADDR, 5, 1001));
        assert!(cookie_eq(&c, &c));
        assert!(!cookie_eq(&c, &cookie(&SECRET, ADDR, 5, 1001)));
    }
}
