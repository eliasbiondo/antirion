//! An RFC 5322 message with a UTF-8 text body, encoded base64 so that every line is ASCII and short.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// A message to one recipient.
#[derive(Clone, Debug)]
pub struct Message {
    pub from: String,
    pub reply_to: Option<String>,
    pub to: String,
    pub subject: String,
    pub body: String,
    /// The `Date` header, already in RFC 5322 form.
    pub date: String,
}

/// Why a message cannot be written.
#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("the {0} field is not a single address")]
    Address(&'static str),
    #[error("the {0} field holds a line break")]
    LineBreak(&'static str),
    #[error("no random source")]
    Random,
}

/// The bare address of a field that may be `Name <addr>`.
pub(crate) fn address(field: &str) -> &str {
    match (field.rfind('<'), field.rfind('>')) {
        (Some(open), Some(close)) if open < close => field.get(open + 1..close).unwrap_or(field).trim(),
        _ => field.trim(),
    }
}

fn single_address(field: &str, name: &'static str) -> Result<(), MessageError> {
    let a = address(field);
    let valid = a.split_once('@').is_some_and(|(l, d)| {
        !l.is_empty() && !d.is_empty() && !d.contains('@') && !a.contains(char::is_whitespace)
    }) && !a.contains(['<', '>', ',', ';']);
    if valid {
        Ok(())
    } else {
        Err(MessageError::Address(name))
    }
}

fn one_line(value: &str, name: &'static str) -> Result<(), MessageError> {
    if value.contains(['\r', '\n']) {
        Err(MessageError::LineBreak(name))
    } else {
        Ok(())
    }
}

/// An RFC 2047 encoded word when the text is not plain ASCII.
fn header_text(text: &str) -> String {
    if text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        text.to_owned()
    } else {
        format!("=?UTF-8?B?{}?=", STANDARD.encode(text))
    }
}

impl Message {
    /// Checks the fields that become headers or envelope addresses.
    pub fn check(&self) -> Result<(), MessageError> {
        one_line(&self.from, "from")?;
        one_line(&self.to, "to")?;
        one_line(&self.subject, "subject")?;
        one_line(&self.date, "date")?;
        single_address(&self.from, "from")?;
        single_address(&self.to, "to")?;
        if let Some(r) = &self.reply_to {
            one_line(r, "reply_to")?;
            single_address(r, "reply_to")?;
        }
        Ok(())
    }

    /// The message as sent after `DATA`, with CRLF line ends and without the final dot.
    pub fn render(&self) -> Result<String, MessageError> {
        self.check()?;
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).map_err(|_| MessageError::Random)?;
        let domain = address(&self.from).rsplit('@').next().unwrap_or("localhost");
        let mut out = String::new();
        out.push_str(&format!("From: {}\r\n", self.from));
        out.push_str(&format!("To: {}\r\n", self.to));
        if let Some(r) = &self.reply_to {
            out.push_str(&format!("Reply-To: {r}\r\n"));
        }
        out.push_str(&format!("Subject: {}\r\n", header_text(&self.subject)));
        out.push_str(&format!("Date: {}\r\n", self.date));
        out.push_str(&format!(
            "Message-ID: <{}@{domain}>\r\n",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
        ));
        out.push_str("MIME-Version: 1.0\r\n");
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
        let body = self.body.replace("\r\n", "\n").replace('\n', "\r\n");
        let encoded = STANDARD.encode(body);
        for chunk in encoded.as_bytes().chunks(76) {
            out.push_str(&String::from_utf8_lossy(chunk));
            out.push_str("\r\n");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> Message {
        Message {
            from: "Antirion <gw@acme.example>".into(),
            reply_to: None,
            to: "alice@acme.example".into(),
            subject: "Réinitialisation".into(),
            body: "Hello\nhttps://gw.acme.example/ui/reset?token=abc\n".into(),
            date: "Mon, 05 Oct 2026 10:00:00 +0000".into(),
        }
    }

    #[test]
    fn renders_headers_and_a_base64_body() {
        let text = msg().render().unwrap();
        assert!(text.starts_with("From: Antirion <gw@acme.example>\r\nTo: alice@acme.example\r\n"));
        assert!(text.contains("Subject: =?UTF-8?B?"));
        assert!(text.contains("Message-ID: <"));
        assert!(text.contains("@acme.example>\r\n"));
        let body = text.split("\r\n\r\n").nth(1).unwrap().replace("\r\n", "");
        let decoded = String::from_utf8(STANDARD.decode(body).unwrap()).unwrap();
        assert_eq!(decoded, "Hello\r\nhttps://gw.acme.example/ui/reset?token=abc\r\n");
        assert!(text.lines().all(|l| l.len() <= 998));
    }

    #[test]
    fn refuses_header_injection_and_lists() {
        let mut m = msg();
        m.subject = "hi\r\nBcc: x@y.z".into();
        assert!(matches!(m.check(), Err(MessageError::LineBreak("subject"))));
        let mut m = msg();
        m.to = "a@b.c, d@e.f".into();
        assert!(matches!(m.check(), Err(MessageError::Address("to"))));
        assert_eq!(address("Name <a@b.c>"), "a@b.c");
    }
}
