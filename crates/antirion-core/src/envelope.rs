//! Which error envelope a data-plane path gets.

/// Error envelope family of a data-plane path, decided from the path alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Envelope {
    OpenAi,
    Anthropic,
    Gemini,
    Bedrock,
    JsonRpc,
}
