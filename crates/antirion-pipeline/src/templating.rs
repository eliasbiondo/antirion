//! The refusals of templated requests: the reasons of the defined template errors, and the generic
//! invalid request of a malformed `template` object.

use antirion_core::Stage;
use antirion_error::{Code, GatewayError, Message};
use antirion_guardrails::template::TemplateError;

use crate::reasons;

/// The refusal of a request whose template could not be applied.
pub(crate) fn refusal(e: &TemplateError) -> GatewayError {
    let quoted = |names: &[String]| {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match e {
        TemplateError::Unknown(name) => GatewayError::with_reason(
            reasons::UNKNOWN_TEMPLATE,
            Stage::GuardInput,
            Message::built(format!(
                "The template {} is not defined.",
                quoted(std::slice::from_ref(name))
            )),
        )
        .param("template.name"),
        TemplateError::Missing(names) => GatewayError::with_reason(
            reasons::TEMPLATE_VARIABLE_MISSING,
            Stage::GuardInput,
            Message::built(format!(
                "These template variables have no value: {}.",
                quoted(names)
            )),
        )
        .param("template.variables"),
        TemplateError::Conflict(member) => GatewayError::with_reason(
            reasons::TEMPLATE_CONFLICT,
            Stage::GuardInput,
            Message::built(format!(
                "A request with a template object cannot also carry `{member}`."
            )),
        )
        .param(*member),
        TemplateError::Required => GatewayError::with_reason(
            reasons::TEMPLATE_REQUIRED,
            Stage::GuardInput,
            Message::Fixed("This route serves templated requests only: the request needs a template object."),
        )
        .param(antirion_guardrails::template::MEMBER),
        TemplateError::Invalid(param, why) => GatewayError::new(
            Code::InvalidRequest,
            Stage::GuardInput,
            Message::built(why.clone()),
        )
        .param(param),
    }
}
