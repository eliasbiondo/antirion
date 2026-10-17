//! Media in requests: data URLs, media-type sniffing, the format lists of a target, and names.

use super::util::b64_decode;

/// A parsed `data:<type>/<subtype>[;params];base64,<data>` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataUrl {
    pub mime: String,
    pub data: String,
}

/// Parses a base64 data URL, checking that the payload decodes.
#[must_use]
pub fn parse_data_url(url: &str) -> Option<DataUrl> {
    let rest = url.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let mut params = head.split(';');
    let mime = params.next()?.trim().to_ascii_lowercase();
    let (kind, sub) = mime.split_once('/')?;
    if kind.is_empty() || sub.is_empty() {
        return None;
    }
    if !params.any(|p| p.trim().eq_ignore_ascii_case("base64")) {
        return None;
    }
    b64_decode(data)?;
    Some(DataUrl {
        mime,
        data: data.to_owned(),
    })
}

/// The image type named by the first bytes of `data` (base64), if any.
#[must_use]
pub fn sniff_image(data: &str) -> Option<&'static str> {
    let head: String = data.chars().take(64).collect();
    let usable = head.len() - head.len() % 4;
    let bytes = b64_decode(head.get(..usable)?)?;
    let at = |i: usize, s: &[u8]| bytes.get(i..i + s.len()) == Some(s);
    if at(0, b"\x89PNG") {
        Some("image/png")
    } else if at(0, b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        Some("image/gif")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Some("image/webp")
    } else if at(4, b"ftyp") && (at(8, b"heic") || at(8, b"heix") || at(8, b"mif1") || at(8, b"heif")) {
        Some("image/heic")
    } else {
        None
    }
}

/// The media type of an image: the declared one unless it is missing, generic or contradicted by
/// the first bytes.
#[must_use]
pub fn image_mime(declared: Option<&str>, data: &str) -> String {
    match sniff_image(data) {
        Some(sniffed) => sniffed.to_owned(),
        None => declared
            .filter(|d| !d.is_empty() && *d != "image/generic")
            .unwrap_or("image/png")
            .to_owned(),
    }
}

/// The subtype of a media type (`image/png` → `png`).
#[must_use]
pub fn subtype(mime: &str) -> &str {
    mime.split_once('/').map_or(mime, |(_, s)| s)
}

/// Whether a format list (entries like `png` or `image/png`) admits a media type.
#[must_use]
pub fn admits(list: &[String], mime: &str) -> bool {
    let sub = subtype(mime);
    list.iter().any(|f| {
        let f = f.to_ascii_lowercase();
        f == mime || f == sub || (f == "jpg" && sub == "jpeg") || (f == "jpeg" && sub == "jpg")
    })
}

/// The image media type of a URL's path extension, for Gemini `fileData`.
#[must_use]
pub fn image_type_of_url(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_owned())?;
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        _ => return None,
    })
}

/// The Converse document format of a media type.
#[must_use]
pub fn converse_document_format(mime: &str) -> &str {
    match mime {
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "text/html" => "html",
        "text/markdown" => "md",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        other => subtype(other),
    }
}

/// A Converse document name: characters outside `[A-Za-z0-9 \-()\[\]]` become `-`, runs of `-`
/// collapse, and an empty result is derived from the index.
#[must_use]
pub fn converse_name(name: Option<&str>, index: usize) -> String {
    let mut out = String::new();
    for c in name.unwrap_or_default().chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '(' | ')' | '[' | ']');
        let c = if ok { c } else { '-' };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    if out.trim_matches('-').trim().is_empty() {
        format!("document-{}", index + 1)
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    #[test]
    fn data_urls() {
        let d = parse_data_url(&format!("data:image/png;base64,{PNG}")).unwrap();
        assert_eq!(d.mime, "image/png");
        assert!(parse_data_url("data:image/png,xx").is_none());
        assert!(parse_data_url("data:image/png;base64,@@@").is_none());
        assert!(parse_data_url("https://x/y.png").is_none());
    }

    #[test]
    fn sniffing_and_names() {
        assert_eq!(image_mime(Some("image/jpeg"), PNG), "image/png");
        assert_eq!(image_mime(Some("image/generic"), PNG), "image/png");
        assert_eq!(converse_name(Some("report.pdf"), 0), "report-pdf");
        assert_eq!(converse_name(Some("my report (v2).pdf"), 0), "my report (v2)-pdf");
        assert_eq!(converse_name(Some("a__b.pdf"), 0), "a-b-pdf");
        assert_eq!(converse_name(None, 1), "document-2");
        assert_eq!(image_type_of_url("https://h/cat.PNG?x=1"), Some("image/png"));
        assert!(admits(&["png".into()], "image/png"));
        assert!(!admits(&["image/png".into()], "image/heic"));
    }
}
