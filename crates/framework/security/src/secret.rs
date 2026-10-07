use std::env;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};

const PREFIX: &str = "enc:v2:";
const NONCE_LEN: usize = 12;

#[derive(Debug)]
pub enum SecretSealError {
    MissingKey,
    Encryption,
}

impl std::fmt::Display for SecretSealError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => formatter.write_str("SECRET_ENCRYPTION_KEY is required"),
            Self::Encryption => formatter.write_str("failed to encrypt secret"),
        }
    }
}

impl std::error::Error for SecretSealError {}

/// Encrypts a database secret with AES-256-GCM. Existing envelopes are kept
/// intact so released `enc:v1` rows remain readable during rolling upgrades;
/// saving the plaintext again upgrades the row to `enc:v2`.
pub fn seal_secret(value: &str) -> Result<String, SecretSealError> {
    if value.is_empty() || value.starts_with("enc:v1:") || value.starts_with(PREFIX) {
        return Ok(value.to_owned());
    }
    let source_key = env::var("SECRET_ENCRYPTION_KEY")
        .ok()
        .filter(|key| key.as_bytes().len() >= 32)
        .ok_or(SecretSealError::MissingKey)?;
    let key_material = digest(&SHA256, source_key.as_bytes());
    let key = UnboundKey::new(&AES_256_GCM, key_material.as_ref())
        .map_err(|_| SecretSealError::Encryption)?;
    let key = LessSafeKey::new(key);
    let mut nonce_bytes = [0_u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| SecretSealError::Encryption)?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut ciphertext = value.as_bytes().to_vec();
    key.seal_in_place_append_tag(nonce, Aad::from(PREFIX.as_bytes()), &mut ciphertext)
        .map_err(|_| SecretSealError::Encryption)?;
    let mut envelope = nonce_bytes.to_vec();
    envelope.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", STANDARD.encode(envelope)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_existing_envelopes_without_needing_a_key() {
        assert_eq!(seal_secret("enc:v1:legacy").unwrap(), "enc:v1:legacy");
        assert_eq!(seal_secret("enc:v2:current").unwrap(), "enc:v2:current");
    }
}
