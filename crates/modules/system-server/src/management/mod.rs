pub mod compat;
mod compat_fields;
mod data_scope;
mod excel;
mod messaging;
mod notify;
pub mod role_binding;
mod shared;
mod user_relations;

use axum::Router;

use crate::SystemState;

pub fn routes() -> Router<SystemState> {
    Router::new().merge(compat::routes())
}
