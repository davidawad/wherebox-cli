//! ULIDs: 26-character, lexicographically sortable item and lot ids
//! (48-bit millisecond timestamp + 80 random bits, Crockford base32).
//!
//! Ids generated within the same millisecond increment the random part, so
//! ids from one process always sort in creation order.

use std::sync::Mutex;

use crate::error::Result;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RANDOM_MASK: u128 = (1 << 80) - 1;

static LAST: Mutex<(u64, u128)> = Mutex::new((0, 0));

/// Encode a 128-bit value as 26 Crockford base32 characters.
pub fn encode(v: u128) -> String {
    (0..26).rev().map(|i| ALPHABET[((v >> (i * 5)) & 0x1f) as usize] as char).collect()
}

/// A new ULID.
pub fn new() -> Result<String> {
    let ms = crate::util::utc_now().timestamp_millis().max(0) as u64;
    let mut bytes = [0u8; 16];
    crate::crypto::fill_random(&mut bytes)?;
    let fresh = u128::from_be_bytes(bytes) & RANDOM_MASK;
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (ms, rand) = if ms <= last.0 { (last.0, (last.1 + 1) & RANDOM_MASK) } else { (ms, fresh) };
    *last = (ms, rand);
    Ok(encode((u128::from(ms) << 80) | rand))
}

/// Whether `s` looks like a ULID (26 Crockford base32 characters).
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| ALPHABET.contains(&b.to_ascii_uppercase())) && s.as_bytes()[0] <= b'7'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_well_formed_and_sorted() {
        let ids: Vec<String> = (0..50).map(|_| new().unwrap()).collect();
        assert!(ids.iter().all(|i| is_ulid(i)), "{ids:?}");
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        assert_eq!(encode(0), "00000000000000000000000000");
        assert!(!is_ulid("not-a-ulid"));
    }
}
