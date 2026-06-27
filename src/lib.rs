//! Lattice — a config-driven MCP shell.
//!
//! Lattice turns existing REST APIs and CLI tools into Model Context Protocol
//! servers using a single declarative config file. See `SPEC.md` for the design.

pub mod config;
pub mod engine;
pub mod error;
pub mod exec;
pub mod mcp;
