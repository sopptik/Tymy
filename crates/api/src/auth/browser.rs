//! Přihlášení v prohlížeči: OAuth2 authorization code flow s PKCE (RFC 7636).
//!
//! Náhrada device code flow pro organizace, které ho blokují podmíněným přístupem.
//! Přihlášení proběhne v systémovém prohlížeči. Kód z návratové adresy získáme dvěma způsoby:
//! - návratová adresa `http://localhost…`: místní server ho převezme sám ([`BrowserLogin::wait_loopback`]),
//! - jinak (výchozí `…/oauth2/nativeclient`): uživatel zkopíruje adresu stránky, na které
//!   prohlížeč skončil, a vloží ji do aplikace ([`BrowserLogin::code_from`]).

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tymy_core::{Error, Result};
use url::Url;

use super::device_code::oauth_check;
use super::token::{TokenResponse, TokenSet};
use crate::Endpoints;
use crate::http::{Http, SendLogged, json, net_err};

/// Jak dlouho čeká místní server na návrat z prohlížeče.
const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Rozběhnuté přihlášení v prohlížeči (adresa k otevření a tajemství PKCE).
#[derive(Clone)]
pub struct BrowserLogin {
    /// Adresa přihlašovací stránky, kterou otevřít v prohlížeči.
    pub url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
}

impl std::fmt::Debug for BrowserLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserLogin")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

fn random_token(bytes: usize) -> String {
    let data: Vec<u8> = (0..bytes).map(|_| rand::random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(data)
}

/// `code_challenge` metodou S256.
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn is_loopback(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
}

/// Připraví přihlášení: vygeneruje PKCE a sestaví adresu přihlašovací stránky.
/// U návratové adresy `http://localhost` bez portu vybere volný port.
pub fn start_browser_login(ep: &Endpoints) -> Result<BrowserLogin> {
    let mut redirect = Url::parse(&ep.redirect_uri)
        .map_err(|e| Error::Auth(format!("neplatná redirect_uri {:?}: {e}", ep.redirect_uri)))?;
    if is_loopback(&redirect) && redirect.port().is_none() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| Error::Auth(format!("nelze najít volný port: {e}")))?
            .port();
        let _ = redirect.set_port(Some(port));
    }
    let redirect_uri = redirect.to_string();
    let verifier = random_token(32);
    let state = random_token(16);
    let mut url = Url::parse(&ep.authorize_url())
        .map_err(|e| Error::Auth(format!("neplatná autorita: {e}")))?;
    url.query_pairs_mut()
        .append_pair("client_id", &ep.client_id)
        .append_pair("response_type", "code")
        .append_pair("response_mode", "query")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &ep.login_scopes())
        .append_pair("code_challenge", &challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        // prohlížeč bývá přihlášený jiným účtem: vždy nabídnout výběr
        .append_pair("prompt", "select_account");
    Ok(BrowserLogin {
        url: url.into(),
        redirect_uri,
        verifier,
        state,
    })
}

impl BrowserLogin {
    /// Převezme kód místní server (návratová adresa `http://localhost…`).
    pub fn uses_loopback(&self) -> bool {
        Url::parse(&self.redirect_uri).is_ok_and(|u| is_loopback(&u))
    }

    /// Kód z adresy, na které prohlížeč skončil.
    ///
    /// `Ok(None)`: text nevypadá jako návrat z tohoto přihlášení (např. jiný obsah schránky).
    /// `Err`: server vrátil chybu (např. blokování podmíněným přístupem) nebo nesedí `state`.
    /// `allow_bare`: přijmout i samotný kód (ručně vložený text, ne schránku).
    pub fn code_from(&self, input: &str, allow_bare: bool) -> Result<Option<String>> {
        let input = input.trim();
        let Ok(url) = Url::parse(input) else {
            let bare = allow_bare
                && input.len() >= 20
                && !input.contains("://")
                && !input.contains(char::is_whitespace);
            return Ok(bare.then(|| input.to_owned()));
        };
        let expected = Url::parse(&self.redirect_uri).ok();
        let same_target = expected.is_some_and(|e| {
            e.scheme() == url.scheme()
                && e.host_str() == url.host_str()
                && e.port_or_known_default() == url.port_or_known_default()
                && e.path().trim_end_matches('/') == url.path().trim_end_matches('/')
        });
        if !same_target {
            return Ok(None);
        }
        // odpověď bývá v query, při response_mode=fragment ve fragmentu
        let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        if let Some(f) = url.fragment() {
            pairs.extend(url::form_urlencoded::parse(f.as_bytes()).into_owned());
        }
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if let Some(err) = get("error") {
            let desc = get("error_description").unwrap_or_default();
            let first = desc.lines().next().unwrap_or_default();
            return Err(Error::Auth(format!("{err}: {first}")));
        }
        let Some(code) = get("code") else {
            return Ok(None);
        };
        if get("state").is_some_and(|s| s != self.state) {
            return Err(Error::Auth(
                "odpověď patří k jinému přihlášení (nesedí state), zkuste to znovu".into(),
            ));
        }
        Ok(Some(code.to_owned()))
    }

    /// Místní server na návratové adrese: počká na návrat z prohlížeče a vrátí kód.
    pub async fn wait_loopback(&self) -> Result<String> {
        let redirect = Url::parse(&self.redirect_uri)
            .map_err(|e| Error::Auth(format!("neplatná redirect_uri: {e}")))?;
        let port = redirect.port_or_known_default().unwrap_or(80);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| Error::Auth(format!("nelze otevřít port {port}: {e}")))?;
        tracing::debug!(port, "čekám na návrat z prohlížeče");
        tokio::time::timeout(LOOPBACK_TIMEOUT, async {
            loop {
                let (mut socket, _) = listener
                    .accept()
                    .await
                    .map_err(|e| Error::Auth(format!("místní server: {e}")))?;
                let target = read_request_target(&mut socket).await.unwrap_or_default();
                let mut origin = redirect.clone();
                origin.set_path("");
                origin.set_query(None);
                let full = format!("{}{}", origin.as_str().trim_end_matches('/'), target);
                let result = self.code_from(&full, false);
                let (status, body) = match &result {
                    Ok(Some(_)) => (
                        "200 OK",
                        "Přihlášení dokončeno. Okno prohlížeče můžete zavřít a vrátit se do Tymy.",
                    ),
                    Err(_) => (
                        "200 OK",
                        "Přihlášení se nezdařilo. Podrobnosti jsou v aplikaci Tymy.",
                    ),
                    Ok(None) => ("404 Not Found", "Nenalezeno."),
                };
                let page = format!(
                    "<!DOCTYPE html><html lang=\"cs\"><meta charset=\"utf-8\"><title>Tymy</title>\
                     <body style=\"font-family:sans-serif;margin:3em\"><p>{body}</p></body></html>"
                );
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
                match result {
                    Ok(Some(code)) => return Ok(code),
                    Err(e) => return Err(e),
                    Ok(None) => {}
                }
            }
        })
        .await
        .map_err(|_| Error::Auth("přihlášení v prohlížeči vypršelo".into()))?
    }
}

/// Přečte první řádek HTTP požadavku a vrátí cíl (`/cesta?query`).
async fn read_request_target(socket: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < 16 * 1024 {
        let n = tokio::time::timeout(Duration::from_secs(10), socket.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let line = String::from_utf8_lossy(&buf);
    let line = line.lines().next()?;
    let mut parts = line.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_owned)
}

/// Vymění kód z prohlížeče za tokeny.
pub async fn redeem_code(
    http: &Http,
    ep: &Endpoints,
    login: &BrowserLogin,
    code: &str,
) -> Result<TokenSet> {
    let resp = http
        .client
        .post(ep.token_url())
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", ep.client_id.as_str()),
            ("code", code),
            ("redirect_uri", login.redirect_uri.as_str()),
            ("code_verifier", login.verifier.as_str()),
            ("scope", &ep.login_scopes()),
        ])
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = oauth_check(resp).await?;
    let r: TokenResponse = json(resp).await?;
    Ok(r.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn login(redirect: &str) -> BrowserLogin {
        let ep = Endpoints {
            redirect_uri: redirect.into(),
            ..Default::default()
        };
        start_browser_login(&ep).unwrap()
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // hodnota spočtená nezávisle (Python: urlsafe_b64encode(sha256(v)) bez „=“)
        assert_eq!(
            challenge("dBjftJeZ4CVP-mJ92K9JDHi9JG1u1rkN6uZw6ZYjtsw"),
            "mL1EVsrPKVBXZKQpOq3X5ZsIcwpqYDN-CBdyrvrQ8FI"
        );
    }

    #[test]
    fn authorize_url_has_pkce_and_state() {
        let l = login("https://login.microsoftonline.com/common/oauth2/nativeclient");
        let url = Url::parse(&l.url).unwrap();
        assert!(url.path().ends_with("/oauth2/v2.0/authorize"));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge"], challenge(&l.verifier));
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], l.state);
        assert!(q["scope"].contains("offline_access"));
        assert!(!l.url.contains(&l.verifier), "verifier nesmí do adresy");
        assert!(!l.uses_loopback());
    }

    #[test]
    fn code_from_pasted_address() {
        let l = login("https://login.microsoftonline.com/common/oauth2/nativeclient");
        let ok = format!(
            "https://login.microsoftonline.com/common/oauth2/nativeclient?code=0.AXYZ&state={}&session_state=s",
            l.state
        );
        assert_eq!(l.code_from(&ok, false).unwrap().as_deref(), Some("0.AXYZ"));
        assert_eq!(
            l.code_from(&format!("  {ok}\n"), false).unwrap().as_deref(),
            Some("0.AXYZ")
        );
        // jiný obsah schránky se ignoruje
        assert_eq!(
            l.code_from("https://example.com/?code=x", false).unwrap(),
            None
        );
        assert_eq!(l.code_from("ahoj světe", false).unwrap(), None);
        assert_eq!(
            l.code_from("0.AXYZabcdefghijklmnopqrst", false).unwrap(),
            None
        );
        // ručně vložený samotný kód
        assert_eq!(
            l.code_from("0.AXYZabcdefghijklmnopqrst", true)
                .unwrap()
                .as_deref(),
            Some("0.AXYZabcdefghijklmnopqrst")
        );
        // cizí state
        let wrong =
            "https://login.microsoftonline.com/common/oauth2/nativeclient?code=c&state=jiny";
        assert!(l.code_from(wrong, false).is_err());
        // chyba od serveru
        let blocked = "https://login.microsoftonline.com/common/oauth2/nativeclient?error=access_denied&error_description=AADSTS53003%3A+Access+has+been+blocked+by+Conditional+Access+policies.%0D%0ATrace+ID%3A+x";
        let e = l.code_from(blocked, false).unwrap_err().to_string();
        assert!(e.contains("AADSTS53003"), "{e}");
        assert!(!e.contains("Trace ID"));
    }

    #[tokio::test]
    async fn loopback_receives_code() {
        let l = login("http://localhost");
        assert!(l.uses_loopback());
        let redirect = Url::parse(&l.redirect_uri).unwrap();
        assert!(redirect.port().is_some(), "port se doplní");
        let waiter = {
            let l = l.clone();
            tokio::spawn(async move { l.wait_loopback().await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let base = format!("http://127.0.0.1:{}", redirect.port().unwrap());
        let client = reqwest::Client::new();
        // požadavek na jinou cestu (favicon) se odmítne a čeká se dál
        let r = client
            .get(format!("{base}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let r = client
            .get(format!("{base}/?code=kod123&state={}", l.state))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.text().await.unwrap().contains("Přihlášení dokončeno"));
        assert_eq!(waiter.await.unwrap().unwrap(), "kod123");
    }

    #[tokio::test]
    async fn redeem_sends_verifier() {
        let server = MockServer::start().await;
        let ep = Endpoints {
            authority: format!("{}/organizations", server.uri()),
            ..Default::default()
        };
        let l = start_browser_login(&ep).unwrap();
        Mock::given(method("POST"))
            .and(path("/organizations/oauth2/v2.0/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=kod"))
            .and(body_string_contains(format!(
                "code_verifier={}",
                l.verifier
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "refresh_token": "rt", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let t = redeem_code(&Http::new().unwrap(), &ep, &l, "kod")
            .await
            .unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
    }
}
