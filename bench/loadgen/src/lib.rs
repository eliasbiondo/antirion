//! The benchmark's load generator: open-loop constant-rate and closed-loop runs over HTTP/1.1 or h2c,
//! latency from the intended send time into HdrHistograms, stream timing, outcome counts.

#![forbid(unsafe_code)]

pub mod conn;
pub mod exec;
pub mod record;
pub mod run;
pub mod sse;

pub use exec::{FetchError, Prepared, Source, exchange, fetch};
pub use record::{Counts, Histograms, MAX_US, histogram};
pub use run::{ClosedLoop, Nodes, OpenLoop, Protocol, RunReport, Window, closed_loop, open_loop};
