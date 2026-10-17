//! Cross-dialect translation through an internal representation: client codecs decode requests and
//! encode responses and streams; upstream codecs encode requests and decode responses and streams.

pub mod decode;
pub mod defined;
pub mod encode;
pub mod envelope;
pub mod ir;
pub mod media;
pub mod names;
pub mod replay;
pub mod resp;
pub mod schema;
pub mod stream;
pub mod target;
pub(crate) mod util;
pub mod warn;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_clients;
