//! Extension points for policies other engines own: limits and budgets per server, tool and agent,
//! and the `mcp_input`/`mcp_output` guardrail stages. With no hook installed every call continues.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use antirion_error::GatewayError;
use serde_json::Value;

/// One routed MCP call, as hooks see it.
#[derive(Debug)]
pub struct CallFacts<'a> {
    pub gateway: &'a str,
    pub server: &'a str,
    pub method: &'a str,
    /// The upstream tool or prompt name, or resource URI.
    pub item: &'a str,
    /// Tool or prompt arguments.
    pub arguments: Option<&'a Value>,
    pub principal: &'a str,
}

/// What a hook decided.
#[derive(Debug)]
pub enum HookVerdict {
    Continue,
    /// Continue with this value in place of the arguments (before the call) or of the result.
    Replace(Value),
    /// Refuse with this error (for example `guardrail_blocked`); the upstream is not called, or its
    /// result is not returned.
    Refuse(GatewayError),
}

/// A boxed `Send` future.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A policy applied to routed MCP calls.
pub trait CallHook: Send + Sync + fmt::Debug {
    /// Before the call is sent upstream; `Replace` rewrites the arguments.
    fn before_call<'a>(&'a self, call: &'a CallFacts<'a>) -> Pending<'a, HookVerdict>;

    /// After a result arrived, before it is returned; `Replace` rewrites the result.
    fn after_result<'a>(&'a self, call: &'a CallFacts<'a>, result: &'a Value) -> Pending<'a, HookVerdict>;

    /// Whether `after_result` can change or refuse a result of `method`, so the result must be
    /// checked before the answer commits.
    fn checks_results(&self, method: &str) -> bool;
}

/// A call counted by limits and budgets: an MCP `tools/call`, `prompts/get` or `resources/read`
/// routed to a configured server, or an RPC to a configured A2A agent.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Counted<'a> {
    Mcp {
        server: &'a str,
        method: &'a str,
        /// The upstream tool or prompt name, or resource URI.
        item: &'a str,
    },
    Agent {
        agent: &'a str,
        method: &'a str,
    },
}

/// The charge of an admitted call. Dropped without [`CallCharge::answered`], it is refunded: the
/// call failed before the server or agent answered.
pub trait CallCharge: Send + fmt::Debug {
    /// The server or agent answered; `ok` when the answer is not a JSON-RPC error.
    fn answered(self: Box<Self>, ok: bool) -> Pending<'static, ()>;
}

/// Limits and budgets of MCP and A2A calls: a reservation before the call is sent.
pub trait CallLimits: Send + Sync + fmt::Debug {
    /// Reserves for `call`; `Err` refuses it (`rate_limited`, `budget_exceeded`, …) and the
    /// upstream is not called.
    fn admit<'a>(
        &'a self,
        call: Counted<'a>,
    ) -> Pending<'a, Result<Option<Box<dyn CallCharge>>, GatewayError>>;
}

/// The hooks installed on this node.
#[derive(Clone, Debug, Default)]
pub struct Hooks {
    /// Limits, budgets and per-call prices per server, tool and agent.
    pub limits: Option<Arc<dyn CallLimits>>,
    /// The `mcp_input` and `mcp_output` guardrail stages.
    pub guardrails: Option<Arc<dyn CallHook>>,
}

impl Hooks {
    /// The arguments to send: unchanged (`None`), replaced, or refused.
    pub(crate) async fn before(&self, call: &CallFacts<'_>) -> Result<Option<Value>, GatewayError> {
        let Some(hook) = &self.guardrails else {
            return Ok(None);
        };
        match hook.before_call(call).await {
            HookVerdict::Continue => Ok(None),
            HookVerdict::Replace(v) => Ok(Some(v)),
            HookVerdict::Refuse(e) => Err(e),
        }
    }

    /// The reservation of a counted call, when limits are installed.
    pub(crate) async fn admit(&self, call: Counted<'_>) -> Result<Option<Box<dyn CallCharge>>, GatewayError> {
        match &self.limits {
            Some(l) => l.admit(call).await,
            None => Ok(None),
        }
    }

    /// The result to return: unchanged (`None`), replaced, or refused.
    pub(crate) async fn after(
        &self,
        call: &CallFacts<'_>,
        result: &Value,
    ) -> Result<Option<Value>, GatewayError> {
        let Some(hook) = &self.guardrails else {
            return Ok(None);
        };
        match hook.after_result(call, result).await {
            HookVerdict::Continue => Ok(None),
            HookVerdict::Replace(v) => Ok(Some(v)),
            HookVerdict::Refuse(e) => Err(e),
        }
    }

    /// Whether results of `method` are checked before they are returned.
    pub(crate) fn checks_results(&self, method: &str) -> bool {
        self.guardrails.as_ref().is_some_and(|h| h.checks_results(method))
    }
}

#[cfg(test)]
mod tests {
    use antirion_core::Stage;
    use antirion_error::{Code, Message};

    use super::*;

    #[derive(Debug)]
    struct Deny;

    impl CallHook for Deny {
        fn before_call<'a>(&'a self, _: &'a CallFacts<'a>) -> Pending<'a, HookVerdict> {
            Box::pin(async {
                HookVerdict::Refuse(GatewayError::new(
                    Code::GuardrailBlocked,
                    Stage::GuardInput,
                    Message::Fixed("blocked"),
                ))
            })
        }

        fn after_result<'a>(&'a self, _: &'a CallFacts<'a>, _: &'a Value) -> Pending<'a, HookVerdict> {
            Box::pin(async { HookVerdict::Continue })
        }

        fn checks_results(&self, _: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn no_hooks_continue_and_a_refusal_stops() {
        let facts = CallFacts {
            gateway: "g",
            server: "s",
            method: "tools/call",
            item: "add",
            arguments: None,
            principal: "k",
        };
        assert!(Hooks::default().before(&facts).await.unwrap().is_none());
        let h = Hooks {
            guardrails: Some(Arc::new(Deny)),
            ..Hooks::default()
        };
        assert_eq!(h.before(&facts).await.unwrap_err().code, Code::GuardrailBlocked);
        assert!(h.after(&facts, &Value::Null).await.unwrap().is_none());
        assert!(!h.checks_results("tools/call"));
    }
}
