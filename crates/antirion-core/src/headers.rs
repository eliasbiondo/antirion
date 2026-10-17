//! Names of the headers the gateway reads or writes. All are lowercase, as HTTP/2 requires.

pub const X_REQUEST_ID: &str = "x-request-id";
pub const REQUEST_ID_ANTHROPIC: &str = "request-id";
pub const X_CLIENT_REQUEST_ID: &str = "x-client-request-id";

pub const RESPONSE_PATH: &str = "x-antirion-response-path";
pub const TEST_MODE: &str = "x-antirion-test-mode";
pub const TEST_DEPLOYMENT: &str = "x-antirion-test-deployment";
pub const PROVIDER: &str = "x-antirion-provider";
pub const MODEL: &str = "x-antirion-model";
pub const DEPLOYMENT: &str = "x-antirion-deployment";
pub const ATTEMPTS: &str = "x-antirion-attempts";
pub const ROUTE: &str = "x-antirion-route";
pub const ROUTE_PATH: &str = "x-antirion-route-path";
pub const UPSTREAM_STATUS: &str = "x-antirion-upstream-status";
pub const LOG_ID: &str = "x-antirion-log-id";
pub const STREAM_MODE: &str = "x-antirion-stream-mode";
pub const TOKEN_COUNT_SOURCE: &str = "x-antirion-token-count-source";
pub const UPSTREAM_REQUEST_ID: &str = "x-antirion-upstream-request-id";
pub const OVERHEAD_MS: &str = "x-antirion-overhead-ms";
pub const DEGRADED: &str = "x-antirion-degraded";
pub const CONFIG_GENERATION: &str = "x-antirion-config-generation";
pub const CONFIG_PROPAGATED: &str = "x-antirion-config-propagated";
pub const API_KEY: &str = "x-antirion-api-key";
/// The provider an upload to the Files API goes to.
pub const FILES_PROVIDER: &str = "x-antirion-files-provider";
pub const VERSION: &str = "x-antirion-version";

/// The build version every crate of the workspace shares, sent as `x-antirion-version`.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const X_SHOULD_RETRY: &str = "x-should-retry";
pub const X_API_KEY: &str = "x-api-key";
pub const AZURE_API_KEY: &str = "api-key";
pub const GOOG_API_KEY: &str = "x-goog-api-key";
pub const X_AMZN_ERROR_TYPE: &str = "x-amzn-errortype";
pub const X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";
pub const X_FRAME_OPTIONS: &str = "x-frame-options";
pub const REFERRER_POLICY: &str = "referrer-policy";
pub const CONTENT_SECURITY_POLICY: &str = "content-security-policy";
pub const CHANGE_REASON: &str = "x-antirion-change-reason";
pub const EXPERIMENT: &str = "x-antirion-experiment";
