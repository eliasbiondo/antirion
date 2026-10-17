//! Exclusive XML Canonicalization 1.0 (with or without comments) of one element's subtree, minus
//! an excluded descendant (the enveloped signature). Only the namespaces an element or its
//! attributes use are written, plus the prefixes of an `InclusiveNamespaces` list, and each only
//! where the output does not already have it in scope.

use std::collections::BTreeMap;

use roxmltree::{Document, Node, NodeId, NodeType};

use super::xml::{element_qname, prefix};

/// How one subtree is canonicalized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub(crate) comments: bool,
    /// Prefixes treated as in inclusive canonicalization; `#default` is the default namespace.
    pub(crate) inclusive: Vec<String>,
    pub(crate) exclude: Option<NodeId>,
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

/// The namespace `p` (empty for the default one) is bound to at `n`; empty when unbound.
fn uri_of<'a>(n: Node<'a, '_>, p: &str) -> &'a str {
    n.namespaces()
        .find(|ns| ns.name().unwrap_or_default() == p)
        .map_or("", roxmltree::Namespace::uri)
}

struct Writer<'d, 'i> {
    doc: &'d Document<'i>,
    opts: &'d Options,
    out: String,
}

impl Writer<'_, '_> {
    fn element(&mut self, n: Node<'_, '_>, rendered: &BTreeMap<String, String>) {
        let qname = element_qname(self.doc, n);
        let mut used: Vec<String> = vec![prefix(qname).to_owned()];
        let src = self.doc.input_text();
        let mut attrs: Vec<(&str, &str, &str, &str)> = Vec::new();
        for a in n.attributes() {
            let raw = src.get(a.range_qname()).unwrap_or_default();
            let p = prefix(raw);
            if !p.is_empty() && p != "xml" {
                used.push(p.to_owned());
            }
            attrs.push((a.namespace().unwrap_or_default(), a.name(), raw, a.value()));
        }
        for p in &self.opts.inclusive {
            used.push(if p == "#default" { String::new() } else { p.clone() });
        }
        used.sort();
        used.dedup();
        let mut mine = rendered.clone();
        let mut decls: Vec<(String, String)> = Vec::new();
        for p in used {
            let uri = uri_of(n, &p);
            let inclusive_unbound = uri.is_empty() && !p.is_empty() && self.opts.inclusive.contains(&p);
            if inclusive_unbound {
                continue;
            }
            let shown = rendered.get(&p).map(String::as_str);
            let undeclared_default = p.is_empty() && uri.is_empty() && shown.is_none_or(str::is_empty);
            if !undeclared_default && shown != Some(uri) {
                decls.push((p.clone(), uri.to_owned()));
                mine.insert(p, uri.to_owned());
            }
        }
        attrs.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        self.out.push('<');
        self.out.push_str(qname);
        for (p, uri) in &decls {
            if p.is_empty() {
                self.out.push_str(" xmlns=\"");
            } else {
                self.out.push_str(" xmlns:");
                self.out.push_str(p);
                self.out.push_str("=\"");
            }
            escape_attr(uri, &mut self.out);
            self.out.push('"');
        }
        for (_, _, raw, value) in attrs {
            self.out.push(' ');
            self.out.push_str(raw);
            self.out.push_str("=\"");
            escape_attr(value, &mut self.out);
            self.out.push('"');
        }
        self.out.push('>');
        for c in n.children() {
            self.node(c, &mine);
        }
        self.out.push_str("</");
        self.out.push_str(qname);
        self.out.push('>');
    }

    fn node(&mut self, n: Node<'_, '_>, rendered: &BTreeMap<String, String>) {
        if Some(n.id()) == self.opts.exclude {
            return;
        }
        match n.node_type() {
            NodeType::Element => self.element(n, rendered),
            NodeType::Text => escape_text(n.text().unwrap_or_default(), &mut self.out),
            NodeType::Comment if self.opts.comments => {
                self.out.push_str("<!--");
                self.out.push_str(n.text().unwrap_or_default());
                self.out.push_str("-->");
            }
            NodeType::PI => {
                if let Some(pi) = n.pi() {
                    self.out.push_str("<?");
                    self.out.push_str(pi.target);
                    if let Some(v) = pi.value.filter(|v| !v.is_empty()) {
                        self.out.push(' ');
                        self.out.push_str(v);
                    }
                    self.out.push_str("?>");
                }
            }
            NodeType::Comment | NodeType::Root => {}
        }
    }
}

/// The canonical form of `apex` and its subtree.
pub(crate) fn canonicalize(doc: &Document<'_>, apex: Node<'_, '_>, opts: &Options) -> String {
    let mut w = Writer {
        doc,
        opts,
        out: String::new(),
    };
    w.element(apex, &BTreeMap::new());
    w.out
}

#[cfg(test)]
mod tests {
    use super::super::xml::parse;
    use super::*;

    fn c14n(xml: &str, apex: &str, opts: &Options) -> String {
        let doc = parse(xml).unwrap();
        let n = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == apex)
            .unwrap();
        canonicalize(&doc, n, opts)
    }

    #[test]
    fn only_used_namespaces_are_written_where_first_used() {
        let xml = r#"<r xmlns:a="urn:a" xmlns:b="urn:b" xmlns="urn:d"><a:x b:k="1" z='2' a:j="&lt;&#9;"><y/><a:w/></a:x></r>"#;
        assert_eq!(
            c14n(xml, "x", &Options::default()),
            r#"<a:x xmlns:a="urn:a" xmlns:b="urn:b" z="2" a:j="&lt;&#x9;" b:k="1"><y xmlns="urn:d"></y><a:w></a:w></a:x>"#
        );
    }

    #[test]
    fn inclusive_prefixes_are_written_on_the_apex() {
        let xml = r#"<r xmlns:a="urn:a" xmlns:xs="urn:xs"><a:x/></r>"#;
        let opts = Options {
            inclusive: vec!["xs".to_owned()],
            ..Options::default()
        };
        assert_eq!(
            c14n(xml, "x", &opts),
            r#"<a:x xmlns:a="urn:a" xmlns:xs="urn:xs"></a:x>"#
        );
    }

    #[test]
    fn the_excluded_node_and_comments_are_left_out() {
        let xml = "<r><x>a<!--c-->b<s>gone</s>&#13;</x></r>";
        let doc = parse(xml).unwrap();
        let x = doc.descendants().find(|n| n.has_tag_name("x")).unwrap();
        let s = doc.descendants().find(|n| n.has_tag_name("s")).unwrap();
        let opts = Options {
            exclude: Some(s.id()),
            ..Options::default()
        };
        assert_eq!(canonicalize(&doc, x, &opts), "<x>ab&#xD;</x>");
        let with = Options {
            comments: true,
            ..opts
        };
        assert_eq!(canonicalize(&doc, x, &with), "<x>a<!--c-->b&#xD;</x>");
    }

    #[test]
    fn an_undeclared_default_namespace_is_written_empty() {
        let xml = r#"<r xmlns="urn:d"><x><y xmlns=""/></x></r>"#;
        assert_eq!(
            c14n(xml, "x", &Options::default()),
            r#"<x xmlns="urn:d"><y xmlns=""></y></x>"#
        );
    }
}
