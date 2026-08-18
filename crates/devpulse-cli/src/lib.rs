//! Library surface for the `devpulse` CLI. The binary is a thin clap wrapper
//! around these commands so tests can drive them without spawning a process.

pub mod agent;
pub mod http;
