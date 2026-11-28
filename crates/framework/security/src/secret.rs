use std::{env, fmt};

use rust_toon_framework_gm::{
    LEGACY_AES_SEALED_PREFIX, LEGACY_SEALED_PREFIX, SM4_SEALED_PREFIX, sm4_seal,
};

#[derive(Debug)]
pub enum SecretSealError {
    MissingKey,
    Encryption,
}

impl fmt::Display for SecretSealError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => formatter.write_str("SECRET_ENCRYPTION_KEY is required"),
            Self::Encryption => formatter.write_str("failed to encrypt secret"),
        }
    }
}

impl std::error::Error for SecretSealError {}

/// Encrypts a database secret with SM4-CBC (国密). Existing envelopes are kept
/// intact so released `enc:v1:`/`enc:v2:` rows remain untouched during rolling
/// upgrades; saving the plaintext again upgrades the row to `enc:sm4:v2:`.
/// Sealed values are never decrypted anywhere in the codebase (reads mask
/// them), so no open path lives here.
pub fn seal_secret(value: &str) -> Result<String, SecretSealError> {
    if value.is_empty()
        || value.starts_with(LEGACY_SEALED_PREFIX)
        || value.starts_with(LEGACY_AES_SEALED_PREFIX)
        || value.starts_with(SM4_SEALED_PREFIX)
    {
        return Ok(value.to_owned());
    }
    let source_key = env::var("SECRET_ENCRYPTION_KEY")
        .ok()
        .filter(|key| key.len() >= 32)
        .ok_or(SecretSealError::MissingKey)?;
    sm4_seal(value, &source_key).map_err(|_| SecretSealError::Encryption)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_existing_envelopes_without_needing_a_key() {
        assert_eq!(seal_secret("enc:v1:legacy").unwrap(), "enc:v1:legacy");
        assert_eq!(seal_secret("enc:v2:current").unwrap(), "enc:v2:current");
        assert_eq!(
            seal_secret("enc:sm4:v2:sealed").unwrap(),
            "enc:sm4:v2:sealed"
        );
    }

    #[test]
    fn requires_the_encryption_key_for_plaintext() {
        // The key is read from the environment; in its absence sealing fails
        // closed instead of writing plaintext. The SM4 seal/open roundtrip is
        // covered by the gm crate's own tests.
        assert!(matches!(
            seal_secret("plain-value").unwrap_err(),
            SecretSealError::MissingKey
        ));
    }
}
