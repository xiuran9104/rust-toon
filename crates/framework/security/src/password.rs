use std::fmt;

use rust_toon_framework_gm::sm3_password_hash;
use rust_toon_framework_gm::sm3_password_verify;

#[derive(Debug, Clone)]
pub struct PasswordPolicy {
    pub minimum_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_number: bool,
    pub require_symbol: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            minimum_length: 12,
            require_uppercase: true,
            require_lowercase: true,
            require_number: true,
            require_symbol: true,
        }
    }
}

impl PasswordPolicy {
    pub fn validate(&self, password: &str) -> Result<(), PasswordError> {
        if password.chars().count() < self.minimum_length {
            return Err(PasswordError::TooShort(self.minimum_length));
        }
        if self.require_uppercase && !password.chars().any(char::is_uppercase) {
            return Err(PasswordError::MissingUppercase);
        }
        if self.require_lowercase && !password.chars().any(char::is_lowercase) {
            return Err(PasswordError::MissingLowercase);
        }
        if self.require_number && !password.chars().any(|character| character.is_ascii_digit()) {
            return Err(PasswordError::MissingNumber);
        }
        if self.require_symbol
            && !password
                .chars()
                .any(|character| !character.is_alphanumeric())
        {
            return Err(PasswordError::MissingSymbol);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct PasswordService {
    policy: PasswordPolicy,
}

impl PasswordService {
    pub fn new(policy: PasswordPolicy) -> Self {
        Self { policy }
    }

    pub fn hash(&self, password: &str) -> Result<String, PasswordError> {
        self.policy.validate(password)?;
        self.hash_secret(password)
    }

    pub fn hash_secret(&self, secret: &str) -> Result<String, PasswordError> {
        sm3_password_hash(secret).map_err(|_| PasswordError::HashingFailed)
    }

    pub fn hash_yudao(&self, password: &str) -> Result<String, PasswordError> {
        self.hash(password)
    }

    pub fn verify(&self, password: &str, encoded_hash: &str) -> Result<bool, PasswordError> {
        Ok(sm3_password_verify(password, encoded_hash))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordError {
    TooShort(usize),
    MissingUppercase,
    MissingLowercase,
    MissingNumber,
    MissingSymbol,
    InvalidHash,
    HashingFailed,
}

impl fmt::Display for PasswordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(length) => write!(
                formatter,
                "password must contain at least {length} characters"
            ),
            Self::MissingUppercase => {
                formatter.write_str("password must contain an uppercase character")
            }
            Self::MissingLowercase => {
                formatter.write_str("password must contain a lowercase character")
            }
            Self::MissingNumber => formatter.write_str("password must contain a number"),
            Self::MissingSymbol => formatter.write_str("password must contain a symbol"),
            Self::InvalidHash => formatter.write_str("stored password hash is invalid"),
            Self::HashingFailed => formatter.write_str("password hashing failed"),
        }
    }
}

impl std::error::Error for PasswordError {}

#[cfg(test)]
mod tests {
    use super::{PasswordError, PasswordService};

    #[test]
    fn hashes_and_verifies_a_strong_password() {
        let service = PasswordService::default();
        let hash = service.hash("Enterprise#123").unwrap();

        assert!(service.verify("Enterprise#123", &hash).unwrap());
        assert!(!service.verify("WrongPassword#123", &hash).unwrap());
        assert_ne!(hash, "Enterprise#123");
        assert!(hash.starts_with("$sm3$"));
    }

    #[test]
    fn rejects_a_weak_password() {
        let error = PasswordService::default().hash("short").unwrap_err();
        assert!(matches!(error, PasswordError::TooShort(12)));
    }

    #[test]
    fn hashes_machine_generated_secrets_without_password_policy() {
        let service = PasswordService::default();
        let hash = service.hash_secret("rt-secret").unwrap();

        assert!(service.verify("rt-secret", &hash).unwrap());
    }

    #[test]
    fn rejects_legacy_non_gm_hash_formats() {
        let service = PasswordService::default();
        // Baseline bcrypt hashes no longer verify after the GM cutover.
        assert!(
            !service
                .verify(
                    "admin123",
                    "$2a$04$.vd8nPeLwxt6hnSzmAoAyul8BOLX7Cib6QhcxRe30rfvrIPQHH1OG",
                )
                .unwrap()
        );
        assert!(!service.verify("whatever", "plaintext").unwrap());
    }
}
