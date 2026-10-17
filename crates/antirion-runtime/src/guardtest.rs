//! The guardrail test endpoint's way into the data plane, which owns guardrail evaluation and the
//! calls guardrails make: the server installs it once at startup.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::Snapshot;

/// What a guardrail is tried on.
#[derive(Clone, Debug)]
pub enum Sample {
    /// One text, as a user message (input) or an assistant answer (output).
    Text(String),
    /// A request (input) or response (output) body.
    Body(Value),
}

/// Why a test could not run.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TestError {
    #[error("no guardrail has that name")]
    Unknown,
    #[error("{0}")]
    Invalid(String),
    #[error("guardrail tests are not available on this node")]
    Unavailable,
}

pub type TestFuture = Pin<Box<dyn Future<Output = Result<Value, TestError>> + Send>>;

/// Runs one guardrail on a sample, as the stage would, and reports its verdict.
pub trait GuardrailTester: Send + Sync + fmt::Debug {
    /// `output` selects the output stage. The answer is `{verdict, category, redacted, spans}`.
    fn test(&self, snap: Arc<Snapshot>, name: String, output: bool, sample: Sample) -> TestFuture;
}

/// The installed tester.
#[derive(Debug, Default)]
pub struct GuardrailTests {
    tester: OnceLock<Arc<dyn GuardrailTester>>,
}

impl GuardrailTests {
    pub fn install(&self, tester: Arc<dyn GuardrailTester>) {
        let _ = self.tester.set(tester);
    }

    #[must_use]
    pub fn tester(&self) -> Option<Arc<dyn GuardrailTester>> {
        self.tester.get().cloned()
    }
}
