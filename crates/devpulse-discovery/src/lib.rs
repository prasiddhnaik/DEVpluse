//! DevPulse platform-facing collectors.
//!
//! Everything that talks to the operating system lives here, behind two async
//! traits ([`ProcessCollector`], [`SocketCollector`]) so that platform
//! differences never leak into the domain layer.
//!
//! Milestone 0 ships one implementation of each:
//!
//! * [`SysinfoProcessCollector`] — process table via `sysinfo`.
//! * [`Netstat2SocketCollector`] — TCP/UDP sockets with owning PIDs via
//!   `netstat2` (`libproc` on macOS, `/proc/net` + fd scan on Linux).
//!
//! Both are blocking underneath, so both run on `spawn_blocking` and never
//! stall the async runtime. Both report how long they took and what they could
//! not see, because missing data must degrade the result rather than fake it.

pub mod error;
pub mod platform;
pub mod process;
pub mod socket;

pub use error::CollectorError;
pub use platform::{PlatformCapabilities, Support, capabilities};
pub use process::{
    Degradations, ObservedProcess, ProcessCollector, ProcessSnapshot, ProcessState,
    SysinfoProcessCollector,
};
pub use socket::{
    Netstat2SocketCollector, ObservedSocket, Protocol, SocketCollector, SocketSnapshot, SocketState,
};
