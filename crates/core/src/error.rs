use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Unknown registry: {0}")]
    UnknownRegistry(String),

    #[error("Package not found: {0}")]
    NotFound(String),

    /// Hosted here, and this caller may see none of it.
    ///
    /// # Why this is not `NotFound`, and not `AccessDenied` either
    ///
    /// It renders as **404** — RFC 0006 and RFC 0011-bis §4.5 both settle that
    /// hidden means absent, and a `403` would confirm the name exists to a
    /// caller who may not know it. So to the client it is a `NotFound`.
    ///
    /// To the *server* it is the opposite. On a Hybrid registry `NotFound` means
    /// "we do not host this, ask upstream", and every fall-through site matches
    /// that variant by name. A package this instance hosts privately must never
    /// take that branch: answering with the public package of the same name is
    /// the dependency-confusion substitution the withholding exists to prevent.
    ///
    /// A distinct variant makes the two facts distinguishable where they differ
    /// and identical where they do not — and it fails **closed** by
    /// construction, because a `match` arm written for `NotFound` does not
    /// catch this one.
    #[error("Package not found: {0}")]
    NotFoundWithheld(String),

    /// This instance does not hold it, and this instance will not dial out
    /// (RFC 0008 §4.4).
    ///
    /// # Why not `NotFound`
    ///
    /// `NotFound` asserts the artifact does not exist, which is false — it
    /// exists, it is simply not here. And on a hybrid registry `NotFound` is
    /// the signal that means *ask upstream*, which is the one thing an
    /// air-gapped instance must never do: reusing it would reintroduce
    /// exactly the confusion [`Self::NotFoundWithheld`] exists to prevent.
    ///
    /// It renders as **503**, which a client already reads as "try later" —
    /// and later, here, means after the next bundle.
    #[error("{registry}: not held by this instance and it will not dial out ({key})")]
    ContentUnavailable { registry: String, key: String },

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Registry error: {0}")]
    Registry(String),

    #[error("Auth error: {0}")]
    Auth(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Access denied: {0}")]
    AccessDenied(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("Integrity check failed: {0}")]
    IntegrityFailure(String),

    #[error("Quota exceeded: {0}")]
    QuotaExceeded(String),

    #[error("Invalid version: {0}")]
    InvalidVersion(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Invalid configuration: {0}")]
    Config(String),

    /// The operation is not part of this registry type's protocol — a capability
    /// probe, not a failure. Callers that have a fallback should match on it
    /// rather than propagate it.
    #[error("Not supported: {0}")]
    NotSupported(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
