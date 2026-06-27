//! The pure translation engine: config + a tool call's input → request/command specs.
//!
//! Everything in this module is **pure** (no I/O), so it is unit-tested without network
//! or processes. [`value`] resolves value expressions, [`body`] fans flat input into a
//! nested request body, [`request`] assembles a full HTTP request spec, [`command`]
//! builds an argv-only CLI invocation, and [`response`] parses and field-filters what a
//! tool returns. The execution layer (T11+) runs these specs against real I/O.

pub mod body;
pub mod command;
pub mod request;
pub mod response;
pub mod value;

pub use body::{build_body, BodyError};
pub use command::{build_command, CommandError, CommandSpec};
pub use request::{build_request, HttpRequestSpec, RequestError};
pub use response::{filter, parse_output, ResponseError};
pub use value::{resolve, resolve_optional, resolve_path, Ctx, ValueError, ValueExpr};
