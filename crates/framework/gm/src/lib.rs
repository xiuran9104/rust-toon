//! 国密（GM, Guomi）primitives for RustToon internal cryptography:
//! SM4-CBC for secret-at-rest sealing, SM3 for digests, HMAC-SM3 for MACs and
//! tokens, and PBKDF2-HMAC-SM3 for password hashing. Legacy non-GM formats
//! (bcrypt/Argon2 hashes, the XOR `enc:v1:` and AES-GCM `enc:v2:` seals) are
//! no longer written; `enc:v1:`/`enc:v2:` values remain passthrough on seal
//! and are never decrypted anywhere in the codebase (reads mask them).

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use libsm::sm4::cipher_mode::{CipherMode, Sm4CipherMode};

/// Prefix of values sealed with SM4-CBC + random IV (base64 of iv||ct).
pub const SM4_SEALED_PREFIX: &str = "enc:sm4:v2:";

/// Pre-GM seal prefixes (`enc:v1:` XOR, `enc:v2:` AES-256-GCM). Sealing keeps
/// these values intact and nothing ever opens them, so they only matter as
/// passthrough markers.
pub const LEGACY_SEALED_PREFIX: &str = "enc:v1:";
pub const LEGACY_AES_SEALED_PREFIX: &str = "enc:v2:";

/// Password hash marker: `$sm3$<iterations>$<salt_hex>$<digest_hex>`.
pub const PASSWORD_HASH_PREFIX: &str = "$sm3$";

/// PBKDF2-HMAC-SM3 iteration count used for new password hashes.
pub const PASSWORD_HASH_ITERATIONS: u32 = 16384;

/// Domain prefix for the SM3-derived per-user identity UUIDs.
pub const IDENTITY_DOMAIN: &str = "rust-toon-user:";

#[derive(Debug)]
pub struct GmError(pub String);

impl std::fmt::Display for GmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GmError {}

fn sm4_key(secret: &str) -> [u8; 16] {
    // SM3(secret) -> first 16 bytes as the SM4 key; stable across runs and
    // independent of the secret's length.
    let digest = sm3_digest(secret.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

/// SM3 digest (hex) of the input.
pub fn sm3_hex(data: &[u8]) -> String {
    hex_encode(&sm3_digest(data))
}

pub(crate) fn sm3_digest(data: &[u8]) -> Vec<u8> {
    use libsm::sm3::hash::Sm3Hash;
    let mut hasher = Sm3Hash::new(data);
    hasher.get_hash().to_vec()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// HMAC built on SM3 (block size 64, output 32 bytes).
pub fn hmac_sm3(key: &[u8], message: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&sm3_digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for (index, byte) in key_block.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner_input = Vec::with_capacity(BLOCK + message.len());
    inner_input.extend_from_slice(&inner_pad);
    inner_input.extend_from_slice(message);
    let inner_digest = sm3_digest(&inner_input);
    let mut outer_input = Vec::with_capacity(BLOCK + 32);
    outer_input.extend_from_slice(&outer_pad);
    outer_input.extend_from_slice(&inner_digest);
    sm3_digest(&outer_input)
}

/// PBKDF2 with HMAC-SM3 as the PRF, producing a single 32-byte block.
pub fn pbkdf2_hmac_sm3(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut block_input = Vec::with_capacity(salt.len() + 4);
    block_input.extend_from_slice(salt);
    block_input.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sm3(password, &block_input);
    let mut output = u.clone();
    for _ in 1..iterations {
        u = hmac_sm3(password, &u);
        for (byte, u_byte) in output.iter_mut().zip(u.iter()) {
            *byte ^= u_byte;
        }
    }
    output
}

/// Hash a password with PBKDF2-HMAC-SM3 and a fresh random salt:
/// `$sm3$<iterations>$<salt_hex>$<digest_hex>`.
pub fn sm3_password_hash(password: &str) -> Result<String, GmError> {
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
    let digest = pbkdf2_hmac_sm3(password.as_bytes(), &salt, PASSWORD_HASH_ITERATIONS);
    Ok(format!(
        "{PASSWORD_HASH_PREFIX}{PASSWORD_HASH_ITERATIONS}${}${}",
        hex_encode(&salt),
        hex_encode(&digest)
    ))
}

/// Verify a password against an `$sm3$...` hash. Any other stored format
/// (including historical bcrypt/Argon2 hashes) simply fails to verify.
pub fn sm3_password_verify(password: &str, encoded: &str) -> bool {
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 5 || !parts[0].is_empty() || parts[1] != "sm3" {
        return false;
    }
    let Ok(iterations) = parts[2].parse::<u32>() else {
        return false;
    };
    if iterations == 0 || iterations > 1_000_000 {
        return false;
    }
    let (Ok(salt), Ok(digest)) = (hex::decode(parts[3]), hex::decode(parts[4])) else {
        return false;
    };
    if salt.is_empty() || digest.len() != 32 {
        return false;
    }
    let computed = pbkdf2_hmac_sm3(password.as_bytes(), &salt, iterations);
    constant_time_eq(&computed, &digest)
}

/// Deterministic 16-byte identifier derived from SM3(input), with RFC 4122
/// version/variant bits set so the bytes render as a valid UUID.
pub fn sm3_uuid_bytes(input: &str) -> [u8; 16] {
    let digest = sm3_digest(input.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

/// Seal a secret with SM4-CBC: output `enc:sm4:v2:<base64(iv||ciphertext)>`.
pub fn sm4_seal(plain: &str, secret: &str) -> Result<String, GmError> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    let mut iv = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
    let key = sm4_key(secret);
    let cipher = Sm4CipherMode::new(&key, CipherMode::Cbc)
        .map_err(|error| GmError(format!("sm4 init: {error:?}")))?;
    let padded = pkcs7_pad(plain.as_bytes(), 16);
    let encrypted = cipher
        .encrypt(&[], &padded, &iv)
        .map_err(|error| GmError(format!("sm4 encrypt: {error:?}")))?;
    let mut blob = iv.to_vec();
    blob.extend_from_slice(&encrypted);
    Ok(format!("{SM4_SEALED_PREFIX}{}", BASE64.encode(blob)))
}

/// Open a sealed secret (`enc:sm4:v2:`). Unsealed/plain values pass through
/// unchanged so tolerant readers keep working; legacy `enc:v1:` values are
/// rejected because the XOR format is retired — use [`legacy_v1_open`] only
/// in an explicit migration pass.
pub fn sm4_open(sealed: &str, secret: &str) -> Result<String, GmError> {
    if let Some(encoded) = sealed.strip_prefix(SM4_SEALED_PREFIX) {
        let blob = BASE64
            .decode(encoded)
            .map_err(|_| GmError("sealed value is not valid base64".into()))?;
        if blob.len() < 32 || blob.len() % 16 != 0 {
            return Err(GmError("sealed value has invalid length".into()));
        }
        let (iv, ciphertext) = blob.split_at(16);
        let key = sm4_key(secret);
        let cipher = Sm4CipherMode::new(&key, CipherMode::Cbc)
            .map_err(|error| GmError(format!("sm4 init: {error:?}")))?;
        let padded = cipher
            .decrypt(&[], ciphertext, iv)
            .map_err(|error| GmError(format!("sm4 decrypt: {error:?}")))?;
        let unpadded =
            pkcs7_unpad(&padded).ok_or_else(|| GmError("padding check failed".into()))?;
        String::from_utf8(unpadded)
            .map_err(|_| GmError("decrypted value is not valid UTF-8".into()))
    } else if sealed.starts_with(LEGACY_SEALED_PREFIX) {
        Err(GmError(
            "legacy enc:v1: values are no longer readable; a migration pass \
             must convert them first"
                .into(),
        ))
    } else {
        // Not sealed (plain) — return as-is so readers stay tolerant.
        Ok(sealed.to_string())
    }
}

/// Migration-only decoder for the retired XOR `enc:v1:` format. Kept solely
/// so a future re-seal pass can convert surviving rows to SM4; new code must
/// never write this format.
pub fn legacy_v1_open(sealed: &str, secret: &str) -> Option<String> {
    let encoded = sealed.strip_prefix(LEGACY_SEALED_PREFIX)?;
    let sealed_bytes = BASE64.decode(encoded).ok()?;
    let key = secret.as_bytes();
    let plain: Vec<u8> = sealed_bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ key[index % key.len()])
        .collect();
    String::from_utf8(plain).ok()
}

/// True when the value is already in the current SM4 sealed format.
pub fn is_sm4_sealed(value: &str) -> bool {
    value.starts_with(SM4_SEALED_PREFIX)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (byte, other) in left.iter().zip(right.iter()) {
        difference |= byte ^ other;
    }
    difference == 0
}

fn pkcs7_pad(data: &[u8], block: usize) -> Vec<u8> {
    let padding = block - (data.len() % block);
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat_n(padding as u8, padding));
    padded
}

fn pkcs7_unpad(data: &[u8]) -> Option<Vec<u8>> {
    let last = *data.last()? as usize;
    if last == 0 || last > 16 || last > data.len() {
        return None;
    }
    let (body, padding) = data.split_at(data.len() - last);
    (padding.iter().all(|byte| *byte as usize == last)).then(|| body.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "unit-test-secret-material";

    #[test]
    fn sm3_known_vector() {
        // GB/T 32905-2016: SM3("abc") starts with 66c7f0f4...
        let digest = sm3_hex(b"abc");
        assert!(digest.starts_with("66c7f0f4"), "got {digest}");
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn hmac_sm3_matches_reference_construction() {
        // HMAC-SM3 must equal the generic HMAC construction over SM3 — verified
        // against an independent byte-for-byte reimplementation.
        let key = b"0123456789012345678901234567890123456789012345678901234567890123";
        let message = b"hmac-sm3 self check";
        let mut inner_pad = [0x36u8; 64];
        let mut outer_pad = [0x5cu8; 64];
        for index in 0..key.len() {
            inner_pad[index] ^= key[index];
            outer_pad[index] ^= key[index];
        }
        let mut inner = inner_pad.to_vec();
        inner.extend_from_slice(message);
        let mut outer = outer_pad.to_vec();
        outer.extend_from_slice(&sm3_digest(&inner));
        let expected = sm3_digest(&outer);
        assert_eq!(hmac_sm3(key, message), expected);
        assert_eq!(hmac_sm3(key, message).len(), 32);
    }

    #[test]
    fn pbkdf2_output_is_deterministic_and_salt_sensitive() {
        let first = pbkdf2_hmac_sm3(b"password", b"salt", 64);
        let second = pbkdf2_hmac_sm3(b"password", b"salt", 64);
        let other_salt = pbkdf2_hmac_sm3(b"password", b"tasl", 64);
        assert_eq!(first, second);
        assert_ne!(first, other_salt);
        // One iteration equals a single HMAC invocation (block index 1).
        assert_eq!(
            pbkdf2_hmac_sm3(b"password", b"salt", 1),
            hmac_sm3(b"password", b"salt\x00\x00\x00\x01")
        );
    }

    #[test]
    fn password_hash_roundtrip() {
        let hash = sm3_password_hash("Enterprise#123").unwrap();
        assert!(hash.starts_with("$sm3$16384$"), "got {hash}");
        assert!(sm3_password_verify("Enterprise#123", &hash));
        assert!(!sm3_password_verify("Enterprise#124", &hash));
        assert!(!sm3_password_verify("Enterprise#123", "not-a-sm3-hash"));
        assert!(!sm3_password_verify(
            "Enterprise#123",
            "$2a$04$.vd8nPeLwxt6hnSzmAoAyul8BOLX7Cib6QhcxRe30rfvrIPQHH1OG"
        ));
    }

    #[test]
    fn sm3_uuid_bytes_is_stable_and_uuid_shaped() {
        let first = sm3_uuid_bytes("rust-toon-user:1");
        let second = sm3_uuid_bytes("rust-toon-user:1");
        let other = sm3_uuid_bytes("rust-toon-user:2");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first[6] >> 4, 4);
        assert_eq!(first[8] >> 6, 0b10);
    }

    #[test]
    fn sm4_seal_open_roundtrip() {
        for plain in ["short", "包含中文的机密", "a".repeat(100).as_str()] {
            let sealed = sm4_seal(plain, SECRET).unwrap();
            assert!(is_sm4_sealed(&sealed));
            assert_ne!(sealed, plain);
            // Random IV: two seals differ.
            let sealed2 = sm4_seal(plain, SECRET).unwrap();
            assert_ne!(sealed, sealed2);
            assert_eq!(sm4_open(&sealed, SECRET).unwrap(), plain);
            assert_eq!(sm4_open(&sealed2, SECRET).unwrap(), plain);
        }
    }

    #[test]
    fn wrong_secret_fails_to_open() {
        let sealed = sm4_seal("sensitive", SECRET).unwrap();
        assert!(sm4_open(&sealed, "other-secret").is_err());
    }

    #[test]
    fn legacy_v1_is_rejected_by_sm4_open_but_decodable_for_migration() {
        use base64::Engine as _;
        let key = SECRET.as_bytes();
        let sealed_bytes: Vec<u8> = b"legacy-secret"
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % key.len()])
            .collect();
        let legacy = format!(
            "{LEGACY_SEALED_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(sealed_bytes)
        );
        assert!(sm4_open(&legacy, SECRET).is_err());
        assert_eq!(legacy_v1_open(&legacy, SECRET).unwrap(), "legacy-secret");
    }

    #[test]
    fn unsealed_passthrough() {
        assert_eq!(sm4_open("plain-value", SECRET).unwrap(), "plain-value");
        assert_eq!(sm4_open("", SECRET).unwrap(), "");
        assert!(!is_sm4_sealed("plain-value"));
    }
}
