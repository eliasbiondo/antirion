//! Compiled guardrail definitions: the closed vocabularies (stages, kinds, actions, categories) and
//! one [`Def`] per entry of `guardrails.definitions`.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::length::LengthCheck;
use crate::limiter::Limiter;
use crate::partners::Partner;
use crate::pattern::{KeywordCheck, RegexCheck};
use crate::preserve::{PiiRules, Preserve};
use crate::rewrite::Rewrite;
use crate::secrets::SecretRules;
use crate::services::{Classifier, Judge};
use crate::structured::JsonSchemaCheck;
use crate::template::Templates;
use crate::tools::ToolRules;
use crate::topic::Topic;

/// The stage a guardrail runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuardStage {
    Input,
    Output,
}

impl GuardStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            GuardStage::Input => "input",
            GuardStage::Output => "output",
        }
    }

    /// The lifecycle stage name (`guard_input`, `guard_output`).
    #[must_use]
    pub const fn lifecycle(self) -> &'static str {
        match self {
            GuardStage::Input => "guard_input",
            GuardStage::Output => "guard_output",
        }
    }
}

/// What a segment of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SegKind {
    System,
    User,
    Assistant,
    Tool,
    ToolDefinition,
    Content,
    Reasoning,
    Refusal,
    ToolArguments,
}

impl SegKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SegKind::System => "system",
            SegKind::User => "user",
            SegKind::Assistant => "assistant",
            SegKind::Tool => "tool",
            SegKind::ToolDefinition => "tool_definition",
            SegKind::Content => "content",
            SegKind::Reasoning => "reasoning",
            SegKind::Refusal => "refusal",
            SegKind::ToolArguments => "tool_arguments",
        }
    }

    pub(crate) fn parse_input(s: &str) -> Option<SegKind> {
        Some(match s {
            "system" => SegKind::System,
            "user" => SegKind::User,
            "assistant" => SegKind::Assistant,
            "tool" => SegKind::Tool,
            "tool_definition" => SegKind::ToolDefinition,
            _ => return None,
        })
    }
}

/// Who wrote a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Client,
    Operator,
    Model,
}

impl Origin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Client => "client",
            Origin::Operator => "operator",
            Origin::Model => "model",
        }
    }
}

/// The closed list of categories a block reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    DenyPattern,
    AllowPattern,
    Keyword,
    Pii,
    Secret,
    Hate,
    Harassment,
    Violence,
    Sexual,
    SelfHarm,
    Illicit,
    PromptInjection,
    OffTopic,
    InvalidJson,
    SchemaViolation,
    ToolNotAllowed,
    Length,
    Uninspectable,
    Custom,
    /// Only in the guardrail object of a guardrail failure.
    Error,
}

const CATEGORIES: [(Category, &str); 20] = [
    (Category::DenyPattern, "deny_pattern"),
    (Category::AllowPattern, "allow_pattern"),
    (Category::Keyword, "keyword"),
    (Category::Pii, "pii"),
    (Category::Secret, "secret"),
    (Category::Hate, "hate"),
    (Category::Harassment, "harassment"),
    (Category::Violence, "violence"),
    (Category::Sexual, "sexual"),
    (Category::SelfHarm, "self_harm"),
    (Category::Illicit, "illicit"),
    (Category::PromptInjection, "prompt_injection"),
    (Category::OffTopic, "off_topic"),
    (Category::InvalidJson, "invalid_json"),
    (Category::SchemaViolation, "schema_violation"),
    (Category::ToolNotAllowed, "tool_not_allowed"),
    (Category::Length, "length"),
    (Category::Uninspectable, "uninspectable"),
    (Category::Custom, "custom"),
    (Category::Error, "error"),
];

impl Category {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        CATEGORIES
            .iter()
            .find(|(c, _)| *c == self)
            .map_or("custom", |(_, s)| s)
    }

    /// A reportable category (never `error`) by name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Category> {
        CATEGORIES
            .iter()
            .find(|(c, name)| *name == s && *c != Category::Error)
            .map(|(c, _)| *c)
    }
}

/// What a finding does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Block,
    Redact,
    Flag,
    /// The request is sent to another Model's route instead.
    Divert,
}

/// How a redacted span is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replacement {
    Label,
    Mask,
    Hash,
}

/// A moderation guardrail's settings.
#[derive(Clone, Debug)]
pub struct Moderation {
    /// The public Model that serves `/v1/moderations`.
    pub model: String,
    pub thresholds: BTreeMap<String, f64>,
    pub max_inputs_per_call: usize,
    pub max_chars: usize,
    pub overlap_chars: usize,
}

/// A webhook guardrail's settings (secret references already resolved).
#[derive(Clone, Debug)]
pub struct Webhook {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Signs each call with `x-antirion-signature: sha256=<hex>` when set.
    pub signing_secret: Option<antirion_core::Secret>,
    /// `mode: body`: the whole request or response is sent, and may be replaced.
    pub body_mode: bool,
}

/// How a forced system prompt is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptMode {
    Replace,
    Prepend,
    Append,
    InsertIfAbsent,
}

/// A `system_prompt` transform.
#[derive(Clone, Debug)]
pub struct SystemPrompt {
    pub text: String,
    pub mode: PromptMode,
    /// `system` or `developer` (OpenAI Chat only).
    pub role: String,
}

/// One decoration message.
#[derive(Clone, Debug)]
pub struct Decoration {
    pub role: String,
    pub content: String,
}

/// A `decorator` transform.
#[derive(Clone, Debug, Default)]
pub struct Decorator {
    pub prepend: Vec<Decoration>,
    pub append: Vec<Decoration>,
    /// Added before the first text of the last user message.
    pub user_prefix: Option<String>,
    /// Added after the last text of the last user message.
    pub user_suffix: Option<String>,
}

/// What a guardrail checks or changes.
#[derive(Debug)]
pub enum Check {
    Regex(RegexCheck),
    Keywords(KeywordCheck),
    Pii(PiiRules),
    Secrets(SecretRules),
    Moderation(Moderation),
    Webhook(Webhook),
    SystemPrompt(SystemPrompt),
    Decorator(Decorator),
    JsonSchema(JsonSchemaCheck),
    ToolPermission(ToolRules),
    Length(LengthCheck),
    Judge(Judge),
    Classifier(Classifier),
    Partner(Partner),
    Template(Templates),
    Rewrite(Rewrite),
    JsonRepair,
    Topic(Topic),
}

/// The rank of a severity name: `critical` 4 down to `low` 1; 0 for anything else.
#[must_use]
pub fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// One compiled guardrail.
#[derive(Debug)]
pub struct Def {
    pub name: String,
    pub check: Check,
    pub action: Action,
    /// `execution: async`.
    pub asynchronous: bool,
    /// `execution: parallel_with_call`: an input check that runs while the upstream call does.
    pub parallel: bool,
    /// `on_error: allow`.
    pub fail_open: bool,
    pub order: i64,
    pub message: Option<String>,
    pub include_operator: bool,
    /// The input kinds in scope; `None` is every kind.
    pub kinds: Option<Vec<SegKind>>,
    /// `scope.messages: last_user`.
    pub last_user: bool,
    pub replacement: Replacement,
    pub timeout: Duration,
    /// Label → category for webhook answers.
    pub categories: BTreeMap<String, Category>,
    /// `critical`, `high`, `medium` or `low`: the configured one, else by action.
    pub severity: String,
    /// Its findings reach `notify.min_severity` and are announced as `guardrail.event`.
    pub notify: bool,
    /// The bound on this node's in-flight evaluations of a networked check (`max_concurrency`).
    pub limiter: Option<Limiter>,
    /// The stages a request that selects it adds it to.
    pub request_stages: Vec<GuardStage>,
    /// The Model a finding sends the request to (`action: divert`).
    pub divert_to: Option<String>,
}

impl Def {
    /// The configured `type`.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self.check {
            Check::Regex(_) => "regex",
            Check::Keywords(_) => "keywords",
            Check::Pii(_) => "pii",
            Check::Secrets(_) => "secrets",
            Check::Moderation(_) => "moderation",
            Check::Webhook(_) => "webhook",
            Check::SystemPrompt(_) => "system_prompt",
            Check::Decorator(_) => "decorator",
            Check::JsonSchema(_) => "json_schema",
            Check::ToolPermission(_) => "tool_permission",
            Check::Length(_) => "length",
            Check::Judge(_) => "llm_judge",
            Check::Classifier(_) => "classifier",
            Check::Partner(ref p) => p.kind.type_name(),
            Check::Template(_) => "template",
            Check::Rewrite(_) => "llm_rewrite",
            Check::JsonRepair => "json_repair",
            Check::Topic(_) => "topic",
        }
    }

    /// The parts of a value its redactions keep.
    #[must_use]
    pub fn keep(&self) -> &[Preserve] {
        match &self.check {
            Check::Pii(p) => &p.preserve,
            _ => &[],
        }
    }

    /// How its redactions replace a match.
    #[must_use]
    pub fn style<'a>(&'a self, hash_key: Option<&'a [u8]>) -> crate::redact::Style<'a> {
        crate::redact::Style {
            how: self.replacement,
            hash_key,
            keep: self.keep(),
        }
    }

    /// The stages the type can run at.
    #[must_use]
    pub fn stages(&self) -> &'static [GuardStage] {
        match self.check {
            Check::SystemPrompt(_)
            | Check::Decorator(_)
            | Check::Template(_)
            | Check::Rewrite(_)
            | Check::Topic(_) => &[GuardStage::Input],
            Check::JsonSchema(_) | Check::JsonRepair => &[GuardStage::Output],
            _ if self.action == Action::Divert => &[GuardStage::Input],
            _ if self.parallel => &[GuardStage::Input],
            _ => &[GuardStage::Input, GuardStage::Output],
        }
    }

    /// A check that reads the whole body rather than its text segments.
    #[must_use]
    pub fn reads_body(&self) -> bool {
        matches!(
            &self.check,
            Check::JsonSchema(_) | Check::ToolPermission(_) | Check::Length(_)
        ) || matches!(&self.check, Check::Webhook(w) if w.body_mode)
    }

    /// A transform (phase A).
    #[must_use]
    pub fn is_transform(&self) -> bool {
        matches!(
            self.check,
            Check::SystemPrompt(_) | Check::Decorator(_) | Check::Template(_) | Check::JsonRepair
        )
    }

    /// A `pii` guardrail whose input tokens are put back into the response.
    #[must_use]
    pub fn reversible(&self) -> bool {
        matches!(&self.check, Check::Pii(p) if p.reversible)
    }

    /// A check that makes a network call.
    #[must_use]
    pub fn networked(&self) -> bool {
        matches!(
            self.check,
            Check::Moderation(_)
                | Check::Webhook(_)
                | Check::Judge(_)
                | Check::Classifier(_)
                | Check::Partner(_)
                | Check::Rewrite(_)
                | Check::Topic(_)
        )
    }

    /// Blocks or redacts on the request path: what streams must enforce.
    #[must_use]
    pub fn enforcing(&self) -> bool {
        !self.is_transform()
            && !self.asynchronous
            && !self.reversible()
            && matches!(self.action, Action::Block | Action::Redact)
    }

    /// The longest match, in code points, when every pattern of this check is bounded.
    #[must_use]
    pub fn window_len(&self) -> Option<usize> {
        match &self.check {
            Check::Regex(r) => r.max_len(),
            Check::Keywords(k) => Some(k.max_len()),
            Check::Pii(p) => p.entities.iter().map(|e| e.max_len()).max(),
            Check::Secrets(r) => r.max_len(),
            _ => None,
        }
    }
}

/// The compiled `guardrails` settings and definitions.
#[derive(Debug, Default)]
pub struct Guardrails {
    pub defs: BTreeMap<String, std::sync::Arc<Def>>,
    pub hash_key: Option<Vec<u8>>,
    pub expose_results: bool,
    pub block_uninspectable: bool,
    /// Some `json_schema` guardrail validates against the request's own schema.
    pub uses_request_schema: bool,
}

impl Guardrails {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&std::sync::Arc<Def>> {
        self.defs.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_round_trip_and_error_is_not_reportable() {
        assert_eq!(Category::parse("self_harm"), Some(Category::SelfHarm));
        assert_eq!(Category::SelfHarm.as_str(), "self_harm");
        assert_eq!(Category::parse("error"), None);
        assert_eq!(Category::Error.as_str(), "error");
        assert_eq!(Category::parse("confidential"), None);
    }
}
