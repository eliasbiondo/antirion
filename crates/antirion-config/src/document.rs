//! Parsing a config document from bytes: strict JSON or YAML 1.2.

use antirion_json::{JsonErrorKind, JsonLimits, PathSeg};
use serde_json::Value;

use crate::{ConfigError, DetailCode, DocPath, yaml};

/// Serialisation of a document.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Yaml,
}

impl Format {
    /// The format named by a `Content-Type`; JSON when absent or unknown.
    #[must_use]
    pub fn from_content_type(ct: Option<&str>) -> Format {
        let essence = ct
            .and_then(|c| c.split(';').next())
            .map(|c| c.trim().to_ascii_lowercase());
        match essence.as_deref() {
            Some("application/yaml" | "application/x-yaml" | "text/yaml" | "text/x-yaml") => Format::Yaml,
            _ => Format::Json,
        }
    }

    /// The format of a file, from its extension.
    #[must_use]
    pub fn from_file_name(name: &str) -> Format {
        if name.to_ascii_lowercase().ends_with(".json") {
            Format::Json
        } else {
            Format::Yaml
        }
    }
}

/// Parses `bytes` into a JSON value whose top level is a mapping. Every error is
/// `malformed_document`, except duplicate keys, which are `duplicate_key` at the duplicate's path.
pub fn parse(bytes: &[u8], format: Format) -> Result<Value, Vec<ConfigError>> {
    let value = match format {
        Format::Json => parse_json(bytes)?,
        Format::Yaml => {
            let text = std::str::from_utf8(bytes).map_err(|_| {
                vec![ConfigError::new(
                    &DocPath::root(),
                    DetailCode::MalformedDocument,
                    "the document is not UTF-8",
                )]
            })?;
            yaml::to_json(text).map_err(|e| {
                let code = if e.duplicate {
                    DetailCode::DuplicateKey
                } else {
                    DetailCode::MalformedDocument
                };
                vec![ConfigError::new(&e.path, code, e.message)]
            })?
        }
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(vec![ConfigError::new(
            &DocPath::root(),
            DetailCode::MalformedDocument,
            "the document must be a mapping",
        )])
    }
}

fn parse_json(bytes: &[u8]) -> Result<Value, Vec<ConfigError>> {
    if let Err(e) = antirion_json::skim(bytes, &JsonLimits { max_depth: 512 }) {
        let mut path = DocPath::root();
        for seg in &e.path {
            match seg {
                PathSeg::Key(k) => path.push_key(k),
                PathSeg::Index(i) => path.push_index(*i),
            }
        }
        let code = if e.kind == JsonErrorKind::DuplicateKey {
            DetailCode::DuplicateKey
        } else {
            DetailCode::MalformedDocument
        };
        return Err(vec![ConfigError::new(&path, code, e.to_string())]);
    }
    serde_json::from_slice(bytes).map_err(|e| {
        vec![ConfigError::new(
            &DocPath::root(),
            DetailCode::MalformedDocument,
            e.to_string(),
        )]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_duplicates_name_their_path() {
        let err = parse(br#"{"deployments": {"d": {}, "d": {}}}"#, Format::Json).unwrap_err();
        assert_eq!(err[0].path, "deployments.d");
        assert_eq!(err[0].code, DetailCode::DuplicateKey);
    }

    #[test]
    fn malformed_and_non_mapping_documents() {
        assert_eq!(
            parse(b"{not json", Format::Json).unwrap_err()[0].code,
            DetailCode::MalformedDocument
        );
        assert_eq!(
            parse(b"[1]", Format::Json).unwrap_err()[0].code,
            DetailCode::MalformedDocument
        );
        assert_eq!(
            parse(b"- a\n", Format::Yaml).unwrap_err()[0].code,
            DetailCode::MalformedDocument
        );
    }

    #[test]
    fn formats_from_content_types_and_names() {
        assert_eq!(
            Format::from_content_type(Some("application/yaml; charset=utf-8")),
            Format::Yaml
        );
        assert_eq!(Format::from_content_type(Some("application/json")), Format::Json);
        assert_eq!(Format::from_content_type(None), Format::Json);
        assert_eq!(Format::from_file_name("gw.JSON"), Format::Json);
        assert_eq!(Format::from_file_name("gw.yaml"), Format::Yaml);
    }
}
