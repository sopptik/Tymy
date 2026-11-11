//! Klient trouteru: WebSocket, přes který server doručuje požadavky (zprávy, přítomnost,
//! události hovorů) na veřejnou adresu klienta (`surl`).
//!
//! Rámce mají tvar socket.io 0.9 `typ:id:endpoint:data`:
//! `1::` spojeno, `5:` událost (`{"name", "args"}`), `6:` potvrzení, `3:` doručený požadavek
//! (`{"id", "method", "url", "headers", "body"}`), na který klient odpoví stavem 200.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tymy_core::{Error, Result};
use url::Url;

pub const TROUTER_SCOPE: &str = "https://ic3.teams.office.com/.default";
/// Verze trouter klienta web aplikace (parametr `tc.cv`), z HAR 2026-11-11.
const TROUTER_CLIENT_VERSION: &str = "2026.40.01.1";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Po takové době klidu pošleme ping (web klient ~15 s; server čeká `timeout=40`).
const PING_AFTER: Duration = Duration::from_secs(15);
/// Bez jakéhokoli rámce (ani odpovědi na ping) považujeme spojení za mrtvé.
const DEAD_AFTER: Duration = Duration::from_secs(50);

/// Co server sdělil po přihlášení.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Veřejná adresa klienta, `https://…:3443/v4/f/<id>/`; na cesty pod ní server doručuje.
    pub surl: String,
    /// Id koncového bodu (UUID tohoto klienta, stejné pro hlášení stavu).
    pub epid: String,
}

/// Požadavek doručený serverem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Cesta za `surl`, např. `unifiedPresenceService` nebo `messaging`.
    pub path: String,
    /// Tělo (už rozbalené, pokud bylo gzip + base64).
    pub body: String,
}

/// Rozebraný rámec.
#[derive(Debug, PartialEq)]
enum Frame {
    Connected,
    Heartbeat,
    Disconnect,
    Event { name: String, args: Value },
    Request(Value),
    Other,
}

fn parse_frame(text: &str) -> Frame {
    let mut parts = text.splitn(4, ':');
    let kind = parts.next().unwrap_or("");
    let data = parts.nth(2).unwrap_or("");
    match kind {
        "0" => Frame::Disconnect,
        "1" => Frame::Connected,
        "2" => Frame::Heartbeat,
        "5" => match serde_json::from_str::<Value>(data) {
            Ok(v) => Frame::Event {
                name: v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                args: v.get("args").cloned().unwrap_or(Value::Null),
            },
            Err(_) => Frame::Other,
        },
        "3" => serde_json::from_str::<Value>(data)
            .map(Frame::Request)
            .unwrap_or(Frame::Other),
        _ => Frame::Other,
    }
}

fn auth_frame(token: &str) -> String {
    let body = json!({ "name": "user.authenticate", "args": [{ "headers": {
        "Authorization": format!("Bearer {token}"),
        "X-MS-Migration": "True",
    }}]});
    format!("5:::{body}")
}

fn event_frame(seq: u64, name: &str, args: Option<&Value>) -> String {
    let mut body = json!({ "name": name });
    if let Some(a) = args {
        body["args"] = a.clone();
    }
    format!("5:{seq}+::{body}")
}

fn header<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    headers
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.as_str())
}

/// Odpověď 200 na doručený požadavek.
fn response_frame(req: &Value) -> String {
    let headers = req.get("headers").unwrap_or(&Value::Null);
    let mut out = json!({ "trouter-client": "{\"cd\": 0}" });
    if let Some(cv) = header(headers, "MS-CV") {
        out["MS-CV"] = json!(format!("{cv}.0"));
    }
    if let Some(tr) = header(headers, "trouter-request") {
        out["trouter-request"] = json!(tr);
    }
    let body = json!({
        "id": req.get("id").cloned().unwrap_or(Value::Null),
        "status": 200,
        "headers": out,
        "body": "",
    });
    format!("3:::{body}")
}

/// Cesta a tělo doručeného požadavku.
fn delivery(req: &Value) -> Option<Delivery> {
    let url = req.get("url")?.as_str()?;
    // `/v4/f/<id>/<cesta>`
    let path = url
        .strip_prefix("/v4/f/")
        .and_then(|r| r.split_once('/'))
        .map(|(_, p)| p)
        .unwrap_or(url);
    let headers = req.get("headers").unwrap_or(&Value::Null);
    let raw = req.get("body").and_then(Value::as_str).unwrap_or("");
    let gzip = header(headers, "X-Microsoft-Skype-Content-Encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let body = if gzip {
        let bytes = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
        let mut s = String::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_string(&mut s)
            .ok()?;
        s
    } else {
        raw.to_owned()
    };
    Some(Delivery {
        path: path.to_owned(),
        body,
    })
}

fn ws_err(e: impl std::fmt::Display) -> Error {
    Error::Network(format!("trouter: {e}"))
}

/// Adresa WebSocketu s parametry klienta.
fn connect_url(base: &str, epid: &str, client_version: &str) -> Result<Url> {
    let tc =
        json!({ "cv": TROUTER_CLIENT_VERSION, "ua": "TeamsCDL", "hr": "", "v": client_version });
    let now = chrono::Utc::now().timestamp_millis();
    Url::parse_with_params(
        base,
        &[
            ("tc", tc.to_string()),
            ("timeout", "40".into()),
            ("epid", epid.to_owned()),
            ("ccid", String::new()),
            ("cor_id", uuid::Uuid::new_v4().to_string()),
            ("con_num", format!("{now}_0")),
        ],
    )
    .map_err(|e| Error::Other(format!("adresa trouteru {base}: {e}")))
}

/// Otevřené spojení.
pub struct Trouter<S> {
    ws: WebSocketStream<S>,
    seq: u64,
    pub info: Info,
}

pub type TlsTrouter = Trouter<tokio_rustls::client::TlsStream<TcpStream>>;

impl TlsTrouter {
    /// Připojí se na `base` (`wss://…/v4/c`) a přihlásí tokenem pro [`TROUTER_SCOPE`].
    pub async fn connect(
        base: &str,
        token: &str,
        client_version: &str,
        epid: &str,
    ) -> Result<Self> {
        let epid = epid.to_owned();
        let url = connect_url(base, &epid, client_version)?;
        if url.scheme() != "wss" {
            return Err(Error::Other("trouter vyžaduje wss://".into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::Other("adresa trouteru bez hostitele".into()))?
            .to_owned();
        let port = url.port().unwrap_or(443);
        let connect = async {
            let tcp = TcpStream::connect((host.as_str(), port))
                .await
                .map_err(ws_err)?;
            let name = ServerName::try_from(host.clone()).map_err(ws_err)?;
            let tls = tokio_rustls::TlsConnector::from(tls_config()?)
                .connect(name, tcp)
                .await
                .map_err(ws_err)?;
            Self::handshake(tls, url.as_str(), token, epid).await
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, connect)
            .await
            .map_err(|_| ws_err("připojení trvá příliš dlouho"))?
    }
}

fn tls_config() -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    let certs = rustls_native_certs::load_native_certs();
    for cert in certs.certs {
        // jednotlivé nečitelné certifikáty přeskočit
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(ws_err("systém nemá kořenové certifikáty"));
    }
    let config = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(ws_err)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

impl<S: AsyncRead + AsyncWrite + Unpin> Trouter<S> {
    /// WebSocket handshake nad hotovým spojením, přihlášení a čekání na `trouter.connected`.
    async fn handshake(stream: S, url: &str, token: &str, epid: String) -> Result<Self> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.into_client_request().map_err(ws_err)?;
        request.headers_mut().insert(
            "Origin",
            "https://teams.cloud.microsoft".parse().map_err(ws_err)?,
        );
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(ws_err)?;
        let mut t = Self {
            ws,
            seq: 0,
            info: Info {
                surl: String::new(),
                epid,
            },
        };
        loop {
            match t.read().await? {
                Frame::Connected => t.send(auth_frame(token)).await?,
                Frame::Event { name, args } if name == "trouter.connected" => {
                    let surl = args
                        .pointer("/0/surl")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::Protocol("trouter.connected bez surl".into()))?;
                    t.info.surl = surl.to_owned();
                    tracing::info!(epid = %t.info.epid, "trouter připojen");
                    return Ok(t);
                }
                Frame::Disconnect => return Err(ws_err("server spojení odmítl")),
                _ => {}
            }
        }
    }

    async fn send(&mut self, text: String) -> Result<()> {
        self.ws.send(Message::text(text)).await.map_err(ws_err)
    }

    /// Další rámec; po [`PING_AFTER`] klidu pošle ping, po [`DEAD_AFTER`] spojení vzdá.
    async fn read(&mut self) -> Result<Frame> {
        let mut pinged = false;
        loop {
            let wait = if pinged {
                DEAD_AFTER - PING_AFTER
            } else {
                PING_AFTER
            };
            match tokio::time::timeout(wait, self.ws.next()).await {
                Err(_) if pinged => return Err(ws_err("server neodpovídá")),
                Err(_) => {
                    self.seq += 1;
                    self.send(event_frame(self.seq, "ping", None)).await?;
                    pinged = true;
                }
                Ok(None) => return Err(ws_err("spojení ukončeno")),
                Ok(Some(Err(e))) => return Err(ws_err(e)),
                Ok(Some(Ok(Message::Text(text)))) => return Ok(parse_frame(text.as_str())),
                Ok(Some(Ok(Message::Close(_)))) => return Err(ws_err("spojení zavřeno")),
                // ping/pong WebSocketu řeší knihovna, binární rámce trouter neposílá
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    /// Počká na další doručený požadavek (potvrdí ho serveru) a vrátí ho.
    pub async fn next(&mut self) -> Result<Delivery> {
        loop {
            match self.read().await? {
                Frame::Request(req) => {
                    self.send(response_frame(&req)).await?;
                    match delivery(&req) {
                        Some(d) => return Ok(d),
                        None => tracing::warn!("trouter: nečitelný požadavek"),
                    }
                }
                Frame::Event { name, args } if name == "trouter.message_loss" => {
                    // server chce potvrdit, že o ztrátě zpráv víme
                    self.seq += 1;
                    let frame =
                        event_frame(self.seq, "trouter.processed_message_loss", Some(&args));
                    self.send(frame).await?;
                }
                Frame::Heartbeat => self.send("2::".into()).await?,
                Frame::Disconnect => return Err(ws_err("server spojení ukončil")),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::net::TcpListener;

    #[test]
    fn parses_frames() {
        assert_eq!(parse_frame("1::"), Frame::Connected);
        assert_eq!(parse_frame("2::"), Frame::Heartbeat);
        assert_eq!(parse_frame("6:::1+[\"pong\"]"), Frame::Other);
        assert_eq!(
            parse_frame(
                r#"5:1::{"name":"trouter.connected","args":[{"surl":"https://x/v4/f/abc/"}]}"#
            ),
            Frame::Event {
                name: "trouter.connected".into(),
                args: json!([{"surl": "https://x/v4/f/abc/"}]),
            }
        );
        // dvojtečky v datech nesmí rámec rozbít
        let Frame::Request(req) =
            parse_frame(r#"3:::{"id":7,"url":"/v4/f/abc/messaging","body":"a:b:c"}"#)
        else {
            panic!("čekal jsem požadavek");
        };
        assert_eq!(req["body"], "a:b:c");
    }

    #[test]
    fn builds_frames() {
        assert_eq!(event_frame(3, "ping", None), r#"5:3+::{"name":"ping"}"#);
        let auth = auth_frame("tok");
        assert!(auth.starts_with("5:::{"));
        let v: Value = serde_json::from_str(&auth[4..]).unwrap();
        assert_eq!(v["name"], "user.authenticate");
        assert_eq!(v["args"][0]["headers"]["Authorization"], "Bearer tok");
        let req = json!({"id": 42, "url": "/v4/f/abc/x", "headers": {
            "MS-CV": "cv.1", "trouter-request": "{\"id\":\"r\"}"}, "body": ""});
        let resp: Value = serde_json::from_str(&response_frame(&req)[4..]).unwrap();
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["status"], 200);
        assert_eq!(resp["headers"]["MS-CV"], "cv.1.0");
        assert_eq!(resp["headers"]["trouter-request"], "{\"id\":\"r\"}");
    }

    #[test]
    fn reads_delivery_plain_and_gzip() {
        let req = json!({"id": 1, "url": "/v4/f/abc/unifiedPresenceService", "headers": {}, "body": "{\"a\":1}"});
        assert_eq!(
            delivery(&req).unwrap(),
            Delivery {
                path: "unifiedPresenceService".into(),
                body: "{\"a\":1}".into()
            }
        );
        let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        gz.write_all(b"{\"b\":2}").unwrap();
        let packed = base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap());
        let req = json!({"id": 2, "url": "/v4/f/abc/callAgent/u/r/call/end/",
            "headers": {"x-microsoft-skype-content-encoding": "gzip"}, "body": packed});
        let d = delivery(&req).unwrap();
        assert_eq!(d.path, "callAgent/u/r/call/end/");
        assert_eq!(d.body, "{\"b\":2}");
    }

    #[test]
    fn connect_url_carries_client_params() {
        let url = connect_url("wss://trouter.example/v4/c", "ep-1", "1415/1").unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["epid"], "ep-1");
        assert_eq!(q["timeout"], "40");
        let tc: Value = serde_json::from_str(&q["tc"]).unwrap();
        assert_eq!(tc["ua"], "TeamsCDL");
        assert_eq!(tc["v"], "1415/1");
    }

    /// Celý průběh proti místnímu WebSocket serveru: přihlášení, doručení, potvrzení.
    #[tokio::test]
    async fn authenticates_and_acknowledges_deliveries() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::text("1::")).await.unwrap();
            let auth = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(auth.as_str().contains("Bearer tok"), "{auth}");
            ws.send(Message::text(
                r#"5:1::{"name":"trouter.connected","args":[{"surl":"https://pub.example:3443/v4/f/abc/"}]}"#,
            ))
            .await
            .unwrap();
            ws.send(Message::text(
                r#"5:2::{"name":"trouter.message_loss","args":[{"droppedIndicators":[]}]}"#,
            ))
            .await
            .unwrap();
            let loss = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(loss.as_str().starts_with("5:1+::"), "{loss}");
            assert!(loss.as_str().contains("trouter.processed_message_loss"));
            ws.send(Message::text(
                r#"3:::{"id":9,"method":"POST","url":"/v4/f/abc/unifiedPresenceService","headers":{"MS-CV":"x.1"},"body":"{\"presence\":[]}"}"#,
            ))
            .await
            .unwrap();
            let ack = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let v: Value = serde_json::from_str(&ack.as_str()[4..]).unwrap();
            assert_eq!(v["id"], 9);
            assert_eq!(v["status"], 200);
        });
        let tcp = TcpStream::connect(addr).await.unwrap();
        let url = format!("ws://{addr}/v4/c?epid=e");
        let mut t = Trouter::handshake(tcp, &url, "tok", "e".into())
            .await
            .unwrap();
        assert_eq!(t.info.surl, "https://pub.example:3443/v4/f/abc/");
        let d = t.next().await.unwrap();
        assert_eq!(d.path, "unifiedPresenceService");
        assert_eq!(d.body, "{\"presence\":[]}");
        server.await.unwrap();
    }
}
