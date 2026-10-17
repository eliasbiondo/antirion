//! The MCP gateway and the A2A proxy: virtual MCP servers built from upstream servers (both protocol
//! eras, namespacing, grants, sessions carried in encrypted tokens), and agents behind one
//! authenticated entry point each.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod a2a;
mod admin;
mod authserver;
mod bridge;
mod caller;
mod clientsse;
mod compile;
mod connect;
mod credentials;
mod discovery;
mod egress;
mod fanout;
mod fingerprint;
mod glob;
mod hooks;
mod http;
mod legacy;
mod link;
mod lists;
mod log;
mod modern;
mod names;
mod oauth;
mod openapi;
mod relay;
mod respond;
mod route;
mod rpc;
mod select;
mod sse;
mod ssetransport;
mod state;
mod stdio;
mod token;
mod toolbox;
mod toolsearch;
mod upstream;
mod when;

pub use admin::{ApproveError, SessionStats, ToolApproval, approve_tools, server_status, session_stats};
pub use authserver::{Helped, authorization_server, register};
pub use caller::Caller;
pub use compile::{McpSnapshot, check_document, compile};
pub use egress::Egress;
pub use hooks::{CallCharge, CallFacts, CallHook, CallLimits, Counted, HookVerdict, Hooks, Pending};
pub use http::{
    Env, McpPath, a2a_requires_credential, before_body, body_limit, precheck, public_base, serve,
};
pub use log::{CallLog, LogFacts, UpstreamCall};
pub use oauth::{audiences, challenge, metadata};
pub use state::{DefinitionChange, McpState};
pub use token::SessionTokenSample;
pub use toolbox::{OfferedTool, Toolbox, function_name};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;

/// Errors a response body can end with.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// The body of every MCP and A2A response.
pub type Body = BoxBody<Bytes, BoxError>;

/// Reasons this crate registers.
pub mod reasons {
    antirion_error::reasons! {
        MCP_SESSION_NOT_FOUND = "mcp_session_not_found" => NotFound,
        UPSTREAM_SESSION_NOT_FOUND = "upstream_session_not_found" => NotFound,
        AGENT_NOT_FOUND = "agent_not_found" => NotFound,
        UNKNOWN_METHOD = "unknown_method" => NotFound,
        UNKNOWN_TOOL = "unknown_tool" => NotFound,
        UNKNOWN_PROMPT = "unknown_prompt" => NotFound,
        UNKNOWN_RESOURCE = "unknown_resource" => NotFound,
        ORIGIN_NOT_ALLOWED = "origin_not_allowed" => Forbidden,
        MCP_NO_SERVER_GRANTED = "mcp_no_server_granted" => Forbidden,
        INVALID_JSONRPC = "invalid_jsonrpc" => InvalidRequest,
        INVALID_PARAMS = "invalid_params" => InvalidRequest,
        INVALID_CURSOR = "invalid_cursor" => InvalidRequest,
        INVALID_HEADER = "invalid_header" => InvalidRequest,
        URL_NOT_ALLOWED = "url_not_allowed" => InvalidRequest,
        MCP_PROTOCOL_VERSION_UNSUPPORTED = "mcp_protocol_version_unsupported" => InvalidRequest,
        MCP_PROTOCOL_VERSION_MISMATCH = "mcp_protocol_version_mismatch" => InvalidRequest,
        MCP_HEADER_MISMATCH = "mcp_header_mismatch" => InvalidRequest,
        MCP_META_INVALID = "mcp_meta_invalid" => InvalidRequest,
        MCP_CLIENT_CAPABILITIES_MISSING = "mcp_client_capabilities_missing" => InvalidRequest,
        MCP_LEGACY_REQUIRED = "mcp_legacy_required" => InvalidRequest,
        MCP_SESSION_REQUIRED = "mcp_session_required" => InvalidRequest,
        SESSION_STORE_UNAVAILABLE = "session_store_unavailable" => DependencyUnavailable,
        MCP_NO_SERVER_AVAILABLE = "mcp_no_server_available" => NoHealthyDeployment,
        MCP_SERVER_FAILED = "mcp_server_failed" => UpstreamUnavailable,
        UPSTREAM_NOT_MCP = "upstream_not_mcp" => UpstreamBadResponse,
        AGENT_CARD_UNUSABLE = "agent_card_unusable" => UpstreamUnavailable,
        UPSTREAM_JSONRPC_ERROR = "upstream_jsonrpc_error" => UpstreamRejected,
        SUBJECT_TOKEN_REQUIRED = "subject_token_required" => Forbidden,
    }
}
