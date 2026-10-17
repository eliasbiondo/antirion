//! Virtual keys in the gateway's key format, derived from the corpus seed and the key's name, so the
//! load generator and the rendered gateway configuration agree without exchanging secrets.

use crate::prng::SplitMix64;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const KID: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

fn base62(mut n: u64, width: usize) -> String {
    let mut digits = Vec::with_capacity(width);
    while n > 0 {
        digits.push(BASE62[usize::try_from(n % 62).unwrap_or(0)]);
        n /= 62;
    }
    while digits.len() < width {
        digits.push(b'0');
    }
    digits.reverse();
    String::from_utf8_lossy(&digits).into_owned()
}

fn checksum(body: &str) -> String {
    base62(u64::from(crc32fast::hash(body.as_bytes())), 6)
}

fn pick(rng: &mut SplitMix64, alphabet: &[u8], len: usize) -> String {
    (0..len)
        .map(|_| {
            let i = usize::try_from(rng.below(alphabet.len() as u64)).unwrap_or(0);
            char::from(alphabet.get(i).copied().unwrap_or(b'0'))
        })
        .collect()
}

/// The secret of the key named `name`: `sk-antirion-<kid>-<secret><checksum>`.
#[must_use]
pub fn secret(seed: u64, name: &str) -> String {
    let mut rng = SplitMix64::named(seed, &format!("key:{name}"));
    let kid = pick(&mut rng, KID, 12);
    let secret = pick(&mut rng, BASE62, 43);
    let body = format!("sk-antirion-{kid}-{secret}");
    let sum = checksum(&body);
    body + &sum
}

/// A well-formed key that no configuration declares.
#[must_use]
pub fn unknown(seed: u64) -> String {
    secret(seed, "\0unknown")
}

/// The environment variable that carries the key named `name` to the gateway.
#[must_use]
pub fn env_var(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("AK_{mapped}")
}

/// The name of the `i`-th key of a generated pool.
#[must_use]
pub fn pool_name(i: usize) -> String {
    format!("k{i:05}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_the_key_format() {
        assert_eq!(base62(0, 6), "000000");
        assert_eq!(base62(61, 2), "0z");
        let k = secret(1, "a");
        assert_eq!(k.len(), 74);
        let (body, sum) = k.split_at(68);
        assert_eq!(checksum(body), sum);
        assert!(k.starts_with("sk-antirion-"));
    }

    #[test]
    fn keys_are_stable_and_distinct() {
        assert_eq!(secret(5, "k000001"), secret(5, "k000001"));
        assert_ne!(secret(5, "k000001"), secret(5, "k000002"));
        assert_ne!(secret(5, "k000001"), secret(6, "k000001"));
        assert_eq!(env_var("bench-restricted"), "AK_bench_restricted");
    }
}
