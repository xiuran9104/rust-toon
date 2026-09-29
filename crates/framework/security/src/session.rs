use std::{future::Future, pin::Pin, sync::Arc};

use crate::{CurrentUser, SecurityError};

/// Application-owned validation, including revocation and current permissions.
pub trait SessionValidator: Send + Sync {
    fn validate<'a>(
        &'a self,
        token: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CurrentUser, SecurityError>> + Send + 'a>>;
}

/// Injected only after database authentication. Long-lived transports can
/// revalidate the same credentials without depending on the System module.
#[derive(Clone)]
pub struct AuthenticatedSession {
    token: String,
    validator: Arc<dyn SessionValidator>,
}

impl AuthenticatedSession {
    pub fn new(token: String, validator: Arc<dyn SessionValidator>) -> Self {
        Self { token, validator }
    }

    pub async fn validate(&self) -> Result<CurrentUser, SecurityError> {
        self.validator.validate(&self.token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use crate::{DataScope, PermissionSet};

    struct Validator { active: AtomicBool }
    impl SessionValidator for Validator {
        fn validate<'a>(&'a self, token: &'a str) -> Pin<Box<dyn Future<Output = Result<CurrentUser, SecurityError>> + Send + 'a>> {
            Box::pin(async move {
                if token != "test-token" || !self.active.load(Ordering::SeqCst) {
                    return Err(SecurityError::InvalidCredentials);
                }
                Ok(CurrentUser { user_id: "test".into(), username: "test".into(), tenant_id: None,
                    role_codes: vec![], permissions: PermissionSet::default(), data_scope: DataScope::SelfOnly })
            })
        }
    }

    #[tokio::test]
    async fn a_cloned_long_lived_session_rechecks_revocation() {
        let validator = Arc::new(Validator { active: AtomicBool::new(true) });
        let session = AuthenticatedSession::new("test-token".into(), validator.clone());
        let connection = session.clone();
        assert!(connection.validate().await.is_ok());
        validator.active.store(false, Ordering::SeqCst);
        assert!(connection.validate().await.is_err());
        assert!(session.validate().await.is_err());
    }
}
