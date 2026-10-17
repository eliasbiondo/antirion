//! One SMTP transaction: greeting, `EHLO`, the TLS upgrade, authentication, the envelope and the
//! data, each answer read within the server's timeout.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::message::address;
use crate::{MailError, Message, Security, Server};

/// The longest answer line read, and the most lines of one answer.
const MAX_LINE: usize = 4096;
const MAX_LINES: usize = 64;

struct Conn<S> {
    io: BufReader<S>,
    timeout: Duration,
}

/// One answer: its code and the text of its lines.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    fn new(io: S, timeout: Duration) -> Conn<S> {
        Conn {
            io: BufReader::new(io),
            timeout,
        }
    }

    async fn read_reply(&mut self) -> Result<Reply, MailError> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(self.timeout, self.io.read_line(&mut line))
                .await
                .map_err(|_| MailError::Timeout)?
                .map_err(|e| MailError::Connect(e.to_string()))?;
            if n == 0 || line.len() > MAX_LINE || lines.len() >= MAX_LINES {
                return Err(MailError::Protocol);
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let code: u16 = line
                .get(..3)
                .and_then(|c| c.parse().ok())
                .ok_or(MailError::Protocol)?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line.get(4..).unwrap_or_default().to_owned());
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), MailError> {
        let io = self.io.get_mut();
        tokio::time::timeout(self.timeout, async {
            io.write_all(data).await?;
            io.flush().await
        })
        .await
        .map_err(|_| MailError::Timeout)?
        .map_err(|e| MailError::Connect(e.to_string()))
    }

    /// Sends one command and reads its answer, which must have one of the `ok` codes.
    async fn command(&mut self, name: &'static str, line: &str, ok: &[u16]) -> Result<Reply, MailError> {
        self.write(format!("{line}\r\n").as_bytes()).await?;
        let reply = self.read_reply().await?;
        if ok.contains(&reply.code) {
            Ok(reply)
        } else {
            Err(MailError::Refused {
                command: name,
                code: reply.code,
            })
        }
    }

    async fn greeting(&mut self) -> Result<(), MailError> {
        let reply = self.read_reply().await?;
        if reply.code == 220 {
            Ok(())
        } else {
            Err(MailError::Refused {
                command: "greeting",
                code: reply.code,
            })
        }
    }

    async fn ehlo(&mut self, name: &str) -> Result<Capabilities, MailError> {
        let reply = self.command("EHLO", &format!("EHLO {name}"), &[250]).await?;
        Ok(Capabilities::parse(&reply.lines))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Capabilities {
    starttls: bool,
    auth: Vec<String>,
}

impl Capabilities {
    fn parse(lines: &[String]) -> Capabilities {
        let mut caps = Capabilities::default();
        for line in lines.iter().skip(1) {
            let mut words = line.split_whitespace();
            match words.next().map(str::to_ascii_uppercase).as_deref() {
                Some("STARTTLS") => caps.starttls = true,
                Some("AUTH") => caps.auth.extend(words.map(str::to_ascii_uppercase)),
                _ => {}
            }
        }
        caps
    }
}

/// Doubles every line's leading dot, so no body line ends the data early (RFC 5321 §4.5.2).
fn dot_stuffed(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for line in text.split_inclusive("\r\n") {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
    }
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out
}

async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut Conn<S>,
    caps: &Capabilities,
    user: &str,
    password: &str,
) -> Result<(), MailError> {
    let refused = |e: MailError| match e {
        MailError::Refused { .. } => MailError::Auth,
        other => other,
    };
    if caps.auth.iter().any(|m| m == "PLAIN") {
        let token = STANDARD.encode(format!("\0{user}\0{password}"));
        conn.command("AUTH", &format!("AUTH PLAIN {token}"), &[235])
            .await
            .map_err(refused)?;
        return Ok(());
    }
    if caps.auth.iter().any(|m| m == "LOGIN") {
        conn.command("AUTH", "AUTH LOGIN", &[334])
            .await
            .map_err(refused)?;
        conn.command("AUTH", &STANDARD.encode(user), &[334])
            .await
            .map_err(refused)?;
        conn.command("AUTH", &STANDARD.encode(password), &[235])
            .await
            .map_err(refused)?;
        return Ok(());
    }
    Err(MailError::Auth)
}

async fn transact<S: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut Conn<S>,
    caps: &Capabilities,
    server: &Server,
    message: &Message,
    data: &str,
) -> Result<(), MailError> {
    if let Some(user) = &server.username {
        authenticate(conn, caps, user, server.password.as_deref().unwrap_or_default()).await?;
    }
    conn.command("MAIL", &format!("MAIL FROM:<{}>", address(&message.from)), &[250])
        .await?;
    conn.command(
        "RCPT",
        &format!("RCPT TO:<{}>", address(&message.to)),
        &[250, 251],
    )
    .await?;
    conn.command("DATA", "DATA", &[354]).await?;
    conn.write(dot_stuffed(data).as_bytes()).await?;
    let reply = conn.read_reply().await?;
    if reply.code != 250 {
        return Err(MailError::Refused {
            command: "DATA",
            code: reply.code,
        });
    }
    let _ = conn.command("QUIT", "QUIT", &[221]).await;
    Ok(())
}

async fn tls<S: AsyncRead + AsyncWrite + Unpin>(
    server: &Server,
    io: S,
) -> Result<tokio_rustls::client::TlsStream<S>, MailError> {
    let name = ServerName::try_from(server.host.clone()).map_err(|e| MailError::Tls(e.to_string()))?;
    let connector = TlsConnector::from(server.tls.clone());
    tokio::time::timeout(server.timeout, connector.connect(name, io))
        .await
        .map_err(|_| MailError::Timeout)?
        .map_err(|e| MailError::Tls(e.to_string()))
}

/// Connects to the first address of the host the guard allows, so the address checked is the one
/// connected to.
async fn connect(server: &Server) -> Result<TcpStream, MailError> {
    let addrs = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await
        .map_err(|e| MailError::Connect(e.to_string()))?;
    let mut last = None;
    let mut refused = false;
    for addr in addrs {
        if server.allowed.as_ref().is_some_and(|allowed| !allowed(addr.ip())) {
            refused = true;
            continue;
        }
        match TcpStream::connect(addr).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = Some(e.to_string()),
        }
    }
    Err(MailError::Connect(match (last, refused) {
        (Some(e), _) => e,
        (None, true) => "the mail server's address is not allowed by the egress rules".to_owned(),
        (None, false) => "the mail server's name has no address".to_owned(),
    }))
}

/// Delivers one message to the server.
pub async fn send(server: &Server, message: &Message) -> Result<(), MailError> {
    let data = message.render()?;
    let tcp = tokio::time::timeout(server.timeout, connect(server))
        .await
        .map_err(|_| MailError::Timeout)??;
    match server.security {
        Security::Tls => {
            let mut conn = Conn::new(tls(server, tcp).await?, server.timeout);
            conn.greeting().await?;
            let caps = conn.ehlo(&server.helo_name).await?;
            transact(&mut conn, &caps, server, message, &data).await
        }
        Security::None => {
            let mut conn = Conn::new(tcp, server.timeout);
            conn.greeting().await?;
            let caps = conn.ehlo(&server.helo_name).await?;
            transact(&mut conn, &caps, server, message, &data).await
        }
        Security::StartTls => {
            let mut plain = Conn::new(tcp, server.timeout);
            plain.greeting().await?;
            let caps = plain.ehlo(&server.helo_name).await?;
            if !caps.starttls {
                let _ = plain.command("QUIT", "QUIT", &[221]).await;
                return Err(MailError::NoStartTls);
            }
            plain.command("STARTTLS", "STARTTLS", &[220]).await?;
            if !plain.io.buffer().is_empty() {
                return Err(MailError::Protocol);
            }
            let mut conn = Conn::new(tls(server, plain.io.into_inner()).await?, server.timeout);
            let caps = conn.ehlo(&server.helo_name).await?;
            transact(&mut conn, &caps, server, message, &data).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;
    use tokio::task::JoinSet;

    use super::*;

    #[test]
    fn capabilities_and_stuffing() {
        let caps = Capabilities::parse(&[
            "mail.example".into(),
            "PIPELINING".into(),
            "STARTTLS".into(),
            "AUTH PLAIN login".into(),
        ]);
        assert!(caps.starttls);
        assert_eq!(caps.auth, vec!["PLAIN", "LOGIN"]);
        assert_eq!(dot_stuffed("a\r\n.b\r\n"), "a\r\n..b\r\n.\r\n");
        assert_eq!(dot_stuffed("a"), "a\r\n.\r\n");
    }

    /// A scripted server: answers each command from `script` and records what it received.
    async fn sink(
        script: Vec<(&'static str, &'static str)>,
        seen: Arc<Mutex<String>>,
        tasks: &mut JoinSet<()>,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tasks.spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"220 sink ready\r\n").await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut in_data = false;
            let mut pending = String::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                seen.lock().await.push_str(&text);
                pending.push_str(&text);
                if in_data {
                    if pending.ends_with("\r\n.\r\n") {
                        in_data = false;
                        pending.clear();
                        s.write_all(b"250 queued\r\n").await.unwrap();
                    }
                    continue;
                }
                while let Some(i) = pending.find("\r\n") {
                    let line: String = pending.drain(..i + 2).collect();
                    let verb = line.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    let answer = script
                        .iter()
                        .find(|(v, _)| *v == verb)
                        .map_or("500 unknown\r\n", |(_, a)| a);
                    s.write_all(answer.as_bytes()).await.unwrap();
                    if verb == "DATA" {
                        in_data = true;
                    }
                    if verb == "QUIT" {
                        return;
                    }
                }
            }
        });
        port
    }

    fn server(port: u16, security: Security) -> Server {
        Server {
            host: "127.0.0.1".into(),
            port,
            security,
            username: Some("u".into()),
            password: Some("p".into()),
            timeout: Duration::from_secs(5),
            helo_name: "antirion".into(),
            tls: crate::default_tls(),
            allowed: None,
        }
    }

    fn message() -> Message {
        Message {
            from: "gw@acme.example".into(),
            reply_to: None,
            to: "alice@acme.example".into(),
            subject: "Test".into(),
            body: ".hidden line\nvisible".into(),
            date: "Mon, 05 Oct 2026 10:00:00 +0000".into(),
        }
    }

    #[tokio::test]
    async fn an_address_the_guard_refuses_is_never_connected_to() {
        let mut s = server(9, Security::None);
        s.allowed = Some(Arc::new(|ip: std::net::IpAddr| !ip.is_loopback()));
        let Err(MailError::Connect(why)) = send(&s, &message()).await else {
            panic!("connected to a refused address");
        };
        assert!(why.contains("not allowed"), "{why}");
    }

    #[tokio::test]
    async fn delivers_with_auth_plain_and_dot_stuffing() {
        let mut tasks = JoinSet::new();
        let seen = Arc::new(Mutex::new(String::new()));
        let port = sink(
            vec![
                ("EHLO", "250-sink\r\n250 AUTH PLAIN\r\n"),
                ("AUTH", "235 ok\r\n"),
                ("MAIL", "250 ok\r\n"),
                ("RCPT", "250 ok\r\n"),
                ("DATA", "354 go\r\n"),
                ("QUIT", "221 bye\r\n"),
            ],
            seen.clone(),
            &mut tasks,
        )
        .await;
        send(&server(port, Security::None), &message()).await.unwrap();
        let seen = seen.lock().await.clone();
        assert!(seen.contains("EHLO antirion\r\n"));
        assert!(seen.contains(&format!("AUTH PLAIN {}\r\n", STANDARD.encode("\0u\0p"))));
        assert!(seen.contains("MAIL FROM:<gw@acme.example>\r\n"));
        assert!(seen.contains("RCPT TO:<alice@acme.example>\r\n"));
        assert!(seen.contains("\r\n.\r\n"));
    }

    #[tokio::test]
    async fn starttls_is_required_when_asked_for() {
        let mut tasks = JoinSet::new();
        let seen = Arc::new(Mutex::new(String::new()));
        let port = sink(
            vec![
                ("EHLO", "250-sink\r\n250 AUTH PLAIN\r\n"),
                ("QUIT", "221 bye\r\n"),
            ],
            seen.clone(),
            &mut tasks,
        )
        .await;
        let err = send(&server(port, Security::StartTls), &message())
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::NoStartTls), "{err:?}");
        assert!(!seen.lock().await.contains("MAIL FROM"));
    }

    #[tokio::test]
    async fn a_refused_recipient_is_reported() {
        let mut tasks = JoinSet::new();
        let seen = Arc::new(Mutex::new(String::new()));
        let port = sink(
            vec![
                ("EHLO", "250 sink\r\n"),
                ("MAIL", "250 ok\r\n"),
                ("RCPT", "550 no such user\r\n"),
            ],
            seen,
            &mut tasks,
        )
        .await;
        let mut s = server(port, Security::None);
        s.username = None;
        let err = send(&s, &message()).await.unwrap_err();
        assert!(matches!(
            err,
            MailError::Refused {
                command: "RCPT",
                code: 550
            }
        ));
        assert!(!err.is_transient());
    }
}
