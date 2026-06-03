use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

/// Shared auth state extracted from config.
#[derive(Clone)]
pub struct AuthState {
    /// API key → tenant_id mapping. Empty = auth disabled.
    pub keys: Arc<HashMap<String, String>>,
}

impl AuthState {
    pub fn new(keys: HashMap<String, String>) -> Self {
        Self {
            keys: Arc::new(keys),
        }
    }

    pub fn disabled() -> Self {
        Self {
            keys: Arc::new(HashMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.keys.is_empty()
    }
}

/// Extract tenant_id from a valid API key.
pub fn resolve_tenant(auth: &AuthState, api_key: &str) -> Option<String> {
    auth.keys.get(api_key).cloned()
}

/// Axum middleware that validates API keys.
/// Extracts the key from `Authorization: Bearer <key>` or `X-API-Key: <key>`.
/// Sets the tenant_id as a request extension.
pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    let auth = request
        .extensions()
        .get::<AuthState>()
        .cloned()
        .unwrap_or_else(AuthState::disabled);

    if !auth.is_enabled() {
        // Auth disabled — use default tenant.
        let mut request = request;
        request
            .extensions_mut()
            .insert(TenantId("default".to_string()));
        return Ok(next.run(request).await);
    }

    // Try Authorization: Bearer <key>
    let api_key = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        // Try X-API-Key: <key>
        .or_else(|| {
            request
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    let api_key = match api_key {
        Some(k) => k,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    match resolve_tenant(&auth, &api_key) {
        Some(tenant_id) => {
            let mut request = request;
            request.extensions_mut().insert(TenantId(tenant_id));
            Ok(next.run(request).await)
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Tenant ID extracted from auth, available as a request extension.
#[derive(Debug, Clone)]
pub struct TenantId(pub String);
