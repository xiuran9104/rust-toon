//! Authentication context and RBAC authorization primitives.

mod authentication;
mod authorization;
mod config;
mod context;
mod error;
mod password;
mod permission;
mod session;
mod token;

pub use authentication::authenticate;
pub use authorization::{AccessDenied, authorize};
pub use config::{SecurityConfig, SecurityConfigError};
pub use context::{CurrentUser, DataScope};
pub use error::SecurityError;
pub use password::{PasswordError, PasswordPolicy, PasswordService};
pub use permission::{InvalidPermission, Permission, PermissionSet};
pub use session::{AuthenticatedSession, SessionValidator};
pub use token::{Claims, TokenService};
