//! Parsing SAML messages: no DTD (so no entities beyond the predefined ones), bounded size, node
//! count and depth, and the element and attribute prefixes as written, which canonicalization needs.

use roxmltree::{Document, Node, NodeType, ParsingOptions};

use super::SamlError;

pub(crate) const PROTOCOL: &str = "urn:oasis:names:tc:SAML:2.0:protocol";
pub(crate) const ASSERTION: &str = "urn:oasis:names:tc:SAML:2.0:assertion";
pub(crate) const METADATA: &str = "urn:oasis:names:tc:SAML:2.0:metadata";
pub(crate) const DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";
pub(crate) const EXC_C14N_NS: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";

/// The most bytes a message may have.
pub(crate) const MAX_BYTES: usize = 512 * 1024;
const MAX_NODES: u32 = 20_000;
/// The most bytes and nodes an identity provider's metadata may have.
pub(crate) const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
const MAX_METADATA_NODES: u32 = 200_000;
const MAX_DEPTH: usize = 48;

/// Parses a message, refusing a DTD, more than a bounded number of nodes, or deep nesting.
pub(crate) fn parse(text: &str) -> Result<Document<'_>, SamlError> {
    parse_within(text, MAX_BYTES, MAX_NODES)
}

/// Parses metadata, under the larger bounds a metadata document may need.
pub(crate) fn parse_metadata(text: &str) -> Result<Document<'_>, SamlError> {
    parse_within(text, MAX_METADATA_BYTES, MAX_METADATA_NODES)
}

fn parse_within(text: &str, max_bytes: usize, max_nodes: u32) -> Result<Document<'_>, SamlError> {
    if text.len() > max_bytes {
        return Err(SamlError::Malformed);
    }
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: max_nodes,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(text, options).map_err(|_| SamlError::Malformed)?;
    let mut depth = vec![0usize; doc.descendants().count() + 1];
    for n in doc.descendants() {
        let d = n
            .parent()
            .map_or(0, |p| depth.get(p.id().get_usize()).copied().unwrap_or(0) + 1);
        if d > MAX_DEPTH {
            return Err(SamlError::Malformed);
        }
        if let Some(slot) = depth.get_mut(n.id().get_usize()) {
            *slot = d;
        }
    }
    Ok(doc)
}

/// Whether `n` is the element `local` in namespace `ns`.
pub(crate) fn is(n: &Node<'_, '_>, ns: &str, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(ns) && n.tag_name().name() == local
}

/// The element children of `n` named `local` in `ns`.
pub(crate) fn children<'a, 'i>(
    n: Node<'a, 'i>,
    ns: &'a str,
    local: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| is(c, ns, local))
}

/// The only element child of `n` named `local` in `ns`; `Ok(None)` when there is none, an error
/// when there are several.
pub(crate) fn only<'a, 'i>(
    n: Node<'a, 'i>,
    ns: &'a str,
    local: &'a str,
) -> Result<Option<Node<'a, 'i>>, SamlError> {
    let mut found = children(n, ns, local);
    let first = found.next();
    if found.next().is_some() {
        return Err(SamlError::Malformed);
    }
    Ok(first)
}

/// The text of an element made only of text: its text nodes joined. A comment or child element
/// inside is refused, so what is read is what was signed.
pub(crate) fn text(n: Node<'_, '_>) -> Result<String, SamlError> {
    let mut out = String::new();
    for c in n.children() {
        match c.node_type() {
            NodeType::Text => out.push_str(c.text().unwrap_or_default()),
            _ => return Err(SamlError::Malformed),
        }
    }
    Ok(out)
}

/// The qualified name `n` was written with, read from the source text.
pub(crate) fn element_qname<'i>(doc: &Document<'i>, n: Node<'_, 'i>) -> &'i str {
    let src = doc.input_text();
    let start = n.range().start + 1;
    let rest = src.get(start..).unwrap_or_default();
    let end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .unwrap_or(rest.len());
    rest.get(..end).unwrap_or_default()
}

/// The prefix of a qualified name; empty without one.
pub(crate) fn prefix(qname: &str) -> &str {
    qname.split_once(':').map_or("", |(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dtds_and_deep_nesting_are_refused() {
        assert_eq!(
            parse("<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>").unwrap_err(),
            SamlError::Malformed
        );
        let deep = format!("{}{}", "<a>".repeat(60), "</a>".repeat(60));
        assert_eq!(parse(&deep).unwrap_err(), SamlError::Malformed);
        assert!(parse("<a><b/></a>").is_ok());
    }

    #[test]
    fn qualified_names_come_from_the_source() {
        let doc = parse("<p:a xmlns:p='urn:x' xmlns='urn:x'><b/><p:c\n/></p:a>").unwrap();
        let names: Vec<&str> = doc
            .descendants()
            .filter(Node::is_element)
            .map(|n| element_qname(&doc, n))
            .collect();
        assert_eq!(names, ["p:a", "b", "p:c"]);
        assert_eq!(prefix("p:a"), "p");
        assert_eq!(prefix("b"), "");
    }

    #[test]
    fn text_with_a_comment_is_refused() {
        let doc = parse("<a>admin@corp.example<!---->.evil.example</a>").unwrap();
        assert_eq!(text(doc.root_element()).unwrap_err(), SamlError::Malformed);
        let doc = parse("<a>x &amp; y</a>").unwrap();
        assert_eq!(text(doc.root_element()).unwrap(), "x & y");
    }
}
