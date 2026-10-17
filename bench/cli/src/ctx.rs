//! What every command shares: paths, topology, the run's output and work directories, the target,
//! the mock, and the windows (standard or overridden).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use antirion_bench_report::model::{CellDef, Kind};

use crate::config::{Paths, ScenarioDef, Topology};
use crate::hosts::{Pid, Running};
pub(crate) use crate::targets::External;

/// The gateway under test.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    Antirion,
    /// Another gateway, configured by its operator from the driver notes; the harness only sends,
    /// and starts it when it is given a command.
    External(Box<External>),
}

impl Target {
    pub(crate) fn name(&self) -> &str {
        match self {
            Target::Antirion => "antirion",
            Target::External(e) => &e.name,
        }
    }
}

/// Overrides of the standard procedure (shortened runs are never publishable).
#[derive(Debug, Clone, Default)]
pub(crate) struct Windows {
    pub(crate) warmup: Option<Duration>,
    pub(crate) measure: Option<Duration>,
    pub(crate) reps: Option<u32>,
}

pub(crate) const WARMUP: Duration = Duration::from_secs(30);
pub(crate) const MEASURE: Duration = Duration::from_secs(120);
pub(crate) const REPS: u32 = 5;
pub(crate) const NIGHTLY_REPS: u32 = 3;

#[derive(Debug)]
pub(crate) struct Ctx {
    pub(crate) paths: Paths,
    pub(crate) topo: Topology,
    pub(crate) out: PathBuf,
    pub(crate) work: PathBuf,
    pub(crate) gateway_bin: PathBuf,
    pub(crate) mock_addr: SocketAddr,
    pub(crate) mock: Mutex<Option<Running>>,
    pub(crate) mock_pid: Option<Pid>,
    pub(crate) scenarios: BTreeMap<String, ScenarioDef>,
    pub(crate) target: Target,
    pub(crate) windows: Windows,
    pub(crate) suite: String,
    pub(crate) admin_token: String,
    pub(crate) commands: Mutex<Vec<Vec<String>>>,
    pub(crate) debug_build: bool,
    /// Instant and wall clock read together, to date histogram windows.
    pub(crate) epoch: (Instant, SystemTime),
}

impl Ctx {
    pub(crate) fn record_command(&self, argv: &[String]) {
        if let Ok(mut c) = self.commands.lock() {
            c.push(argv.to_vec());
        }
    }

    pub(crate) fn wall(&self, t: Instant) -> SystemTime {
        let (i0, w0) = self.epoch;
        if t >= i0 { w0 + (t - i0) } else { w0 - (i0 - t) }
    }

    pub(crate) fn warmup(&self, def: &CellDef) -> Duration {
        self.windows
            .warmup
            .or_else(|| def.warmup_s.map(Duration::from_secs_f64))
            .unwrap_or(WARMUP)
    }

    pub(crate) fn measure(&self, def: &CellDef) -> Duration {
        self.windows
            .measure
            .or_else(|| def.measure_s.map(Duration::from_secs_f64))
            .unwrap_or(MEASURE)
    }

    pub(crate) fn reps(&self, def: &CellDef) -> u32 {
        let standard = if def.repetitions > 0 {
            def.repetitions
        } else if self.suite == "nightly" {
            NIGHTLY_REPS
        } else {
            REPS
        };
        match def.kind {
            Kind::Micro => 1,
            _ => self.windows.reps.unwrap_or(standard).max(1),
        }
    }

    /// Why results of this run cannot be published, if they cannot.
    pub(crate) fn not_publishable(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.debug_build {
            out.push("the harness was built without optimisations".to_owned());
        }
        if self.windows.warmup.is_some() || self.windows.measure.is_some() || self.windows.reps.is_some() {
            out.push("warm-up, measurement or repetitions differ from the standard procedure".to_owned());
        }
        if !self.topo.reference {
            out.push(format!("topology {} is not a reference topology", self.topo.name));
        }
        out
    }
}
