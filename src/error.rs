//! Top-level error type for the lattice library and binary.
//!
//! Module-level code uses typed errors; this enum is the aggregation point at the
//! binary edge. Later tasks add `Config`, `Engine`, and `Exec` variants.

use thiserror::Error;

/// Errors surfaced by the lattice library.
#[derive(Debug, Error)]
pub enum LatticeError {
    /// A configuration could not be loaded or validated.
    #[error("configuration error: {0}")]
    Config(String),

    /// An underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// Any other error, carried opaquely at the binary edge.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Convenience alias for results that fail with [`LatticeError`].
pub type Result<T> = std::result::Result<T, LatticeError>;
