//! The benchmark mock upstream: OpenAI Chat Completions and embeddings, Anthropic Messages and MCP
//! streamable HTTP, answered from pre-rendered bodies under fixed latency profiles, with no recording.

#![forbid(unsafe_code)]

pub mod profile;
pub mod responses;
pub mod server;
pub mod timer;

pub use profile::Profile;
pub use server::{Stats, serve};
