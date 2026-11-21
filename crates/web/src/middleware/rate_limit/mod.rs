//! Actix-web middleware that enforces per-registry rate limits.
//!
//! # Architecture
//!
//! ```text
//! RateLimitMiddlewareFactory
//!     └── wraps every incoming ServiceRequest
//!         ├── extracts the registry name from the path (/proxy/{registry}/…)
//!         ├── extracts the user key and group membership from the Identity extension
//!         │   (set earlier by AuthMiddleware)
//!         └── delegates to RateLimitService::check()
//!             ├── None  → no rate-limit config for this registry, pass through
//!             ├── Ok(limit)  → allowed, annotate response with X-RateLimit-Limit
//!             └── Err(wait, limit, enforcement, reset_unix)
//!                 ├── Block → 429 with Retry-After / X-RateLimit-Reset headers
//!                 └── Warn  → forward but add X-RateLimit-Warning header
//! ```
//!
//! # Middleware registration order
//!
//! Actix-web wraps are applied in reverse registration order (last `.wrap()` = outermost =
//! first to execute). `AuthMiddleware` **must** be registered **after**
//! `RateLimitMiddlewareFactory` so that it runs first and populates `Identity` before
//! the rate-limit check reads it.

mod middleware;
mod store;

pub use middleware::{extract_registry_from_path, RateLimitMiddlewareFactory};
pub use store::{RateLimitScope, RateLimitService, RateLimitUsage};

#[cfg(test)]
mod tests;
