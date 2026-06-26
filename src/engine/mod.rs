//! The pure translation engine: config + a tool call's input → request/command specs.
//!
//! Everything in this module is **pure** (no I/O), so it is unit-tested without network
//! or processes. Later tasks add the nested body builder (T7), HTTP request builder
//! (T8), CLI command builder (T9), and response filter (T10) on top of [`value`].

pub mod value;

pub use value::{resolve, resolve_optional, resolve_path, Ctx, ValueError, ValueExpr};
