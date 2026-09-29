use axum::{
    extract::{Query, Request, State},
    http::{HeaderMap, Uri, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};
use rust_toon_framework_security::{AuthenticatedSession, CurrentUser, SecurityError, SessionValidator};
use serde::Deserialize;
use std::{future::Future, pin::Pin, sync::Arc};
use uuid::Uuid;

use crate::{SystemState, cache, infrastructure};

#[derive(Clone)]
pub struct DatabaseAuthState {
    state: SystemState,
}

impl DatabaseAuthState {
    pub(crate) fn new(state: SystemState) -> Self {
        Self { state }
    }
}

/// Validates bearer tokens against Yudao storage and replaces JWT-embedded
/// authorization data with the user's current roles and permissions.
/// Requests without a bearer token continue so public routes remain public;
/// their route-level authentication still rejects missing credentials where required.
pub async fn authenticate_from_database(
    State(auth): State<DatabaseAuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, SecurityError> {
    let Some(token) = request_token(request.headers(), request.uri())? else {
        return Ok(next.run(request).await);
    };
    let current_user = auth.validate(&token).await?;
    request.extensions_mut().insert(current_user);
    request.extensions_mut().insert(AuthenticatedSession::new(token, Arc::new(auth)));
    Ok(next.run(request).await)
}

// Browser WebSocket and media elements cannot set Authorization. Accept query
// credentials only on these routes, and never let them override a bearer token.
fn request_token(headers: &HeaderMap, uri: &Uri) -> Result<Option<String>, SecurityError> {
    let bearer = headers.get(AUTHORIZATION).map(|value| {
        value.to_str().ok().and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| !value.is_empty()).map(str::to_owned)
            .ok_or(SecurityError::InvalidCredentials)
    }).transpose()?;
    let path = uri.path();
    let query_route = path.starts_with("/toonflow/assets/files/")
        || matches!(path, "/socket/scriptAgent" | "/socket/productionAgent"
            | "/api/socket/scriptAgent" | "/api/socket/productionAgent");
    #[derive(Deserialize)]
    struct Credentials { token: Option<String> }
    let query = if query_route {
        Query::<Credentials>::try_from_uri(uri)
            .map_err(|_| SecurityError::InvalidCredentials)?.0.token
    } else { None };
    if query.as_deref() == Some("") || matches!((&bearer, &query), (Some(a), Some(b)) if a != b) {
        return Err(SecurityError::InvalidCredentials);
    }
    Ok(bearer.or(query))
}

impl SessionValidator for DatabaseAuthState {
    fn validate<'a>(&'a self, token: &'a str) -> Pin<Box<dyn Future<Output = Result<CurrentUser, SecurityError>> + Send + 'a>> {
        Box::pin(validate_session(self, token))
    }
}

async fn validate_session(auth: &DatabaseAuthState, token: &str) -> Result<CurrentUser, SecurityError> {
    let claims = auth.state.tokens.verify_access_token(token)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| SecurityError::InvalidCredentials)?;

    let is_active = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
             FROM system_oauth2_access_token access
             JOIN system_users users ON users.id = access.user_id
             WHERE md5(access.access_token) = md5($1)
               AND access.access_token = $1
               AND md5('yudao-user:' || access.user_id::text)::uuid = $2
               AND access.deleted = 0
               AND access.expires_time > now()
               AND users.deleted = 0
               AND users.status = 0
         )",
    )
    .bind(token)
    .bind(user_id)
    .fetch_one(&auth.state.pool)
    .await
    .map_err(|_| SecurityError::InvalidCredentials)?;
    if !is_active {
        return Err(SecurityError::InvalidCredentials);
    }

    let account = infrastructure::find_account_by_id(&auth.state.pool, user_id)
        .await
        .map_err(|_| SecurityError::InvalidCredentials)?
        .ok_or(SecurityError::InvalidCredentials)?;
    let current_user: CurrentUser = cache::load_current_user(&auth.state, &account)
        .await
        .map_err(|_| SecurityError::InvalidCredentials)?;
    Ok(current_user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_credentials_are_scoped_and_unambiguous() {
        let headers = HeaderMap::new();
        for path in ["/socket/scriptAgent", "/api/socket/productionAgent", "/toonflow/assets/files/example.png"] {
            let uri = format!("{path}?token=test%2Btoken").parse().unwrap();
            assert_eq!(request_token(&headers, &uri).unwrap().as_deref(), Some("test+token"));
        }
        let unrelated = "/system/user/page?token=test".parse().unwrap();
        assert!(request_token(&headers, &unrelated).unwrap().is_none());
        for query in ["token=one&token=two", "token="] {
            assert!(request_token(&headers, &format!("/socket/scriptAgent?{query}").parse().unwrap()).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer one".parse().unwrap());
        assert!(request_token(&headers, &"/socket/scriptAgent?token=two".parse().unwrap()).is_err());
        assert_eq!(request_token(&headers, &"/socket/scriptAgent?token=one".parse().unwrap()).unwrap().as_deref(), Some("one"));
    }
}
