//! The package documents, embedded at build time so the `README.md` and
//! `AGENTS.md` routes serve exactly what ships in the package. Route sources
//! are `include!`d into generated crates under `target/`, so the embedding
//! has to live here in the shared crate where the relative path is stable.

pub const README_MD: &str = include_str!("../../README.md");
pub const AGENTS_MD: &str = include_str!("../../AGENTS.md");
