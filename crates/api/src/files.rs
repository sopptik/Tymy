//! Stažení přílohy ze zprávy přímo do souboru, bez otevírání v prohlížeči.
//!
//! Přílohy pracovních účtů leží na SharePointu / OneDrive (`properties.files` → `shareUrl`
//! nebo `objectUrl`). Stahujeme je přes Graph `GET /v1.0/shares/{id}/driveItem/content`,
//! kde `id` = `u!` + base64url(URL bez paddingu); token Graph má `Files.ReadWrite.All`
//! (ověřeno 2026-11-26). Graph odpoví `302` na předem podepsanou adresu stahování
//! (`*.sharepoint.com/…/download.aspx?…tempauth=…`), kterou stáhneme **bez** tokenu.

use std::path::Path;
use std::time::Duration;

use base64::Engine;
use tokio::io::AsyncWriteExt;
use tymy_core::{Error, Result};
use url::Url;

use crate::http::{Http, SendLogged, check, net_err};

/// Na stažení velkého souboru nestačí výchozí timeout klienta.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Je to soubor na SharePointu / OneDrive (stahuje se přes Graph)?
pub fn is_sharepoint(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str()
                .is_some_and(|h| h.ends_with(".sharepoint.com") || h.ends_with(".sharepoint.de"))
    })
}

/// Cesta Graph pro obsah souboru podle jeho (sdílecí) adresy.
pub fn share_content_path(url: &str) -> String {
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_bytes());
    format!("/v1.0/shares/u!{id}/driveItem/content")
}

/// Kam Graph přesměroval: jen https na SharePoint / OneDrive.
fn redirect_target(location: &str) -> Result<Url> {
    let u = Url::parse(location).map_err(|e| Error::Protocol(format!("adresa stažení: {e}")))?;
    let ok = u.scheme() == "https"
        && u.host_str().is_some_and(|h| {
            h.ends_with(".sharepoint.com")
                || h.ends_with(".sharepoint.de")
                || h.ends_with(".files.1drv.com")
                || h.ends_with(".svc.ms")
        });
    if !ok && !(cfg!(test) && u.host_str() == Some("127.0.0.1")) {
        return Err(Error::Protocol(format!(
            "stažení přesměrováno mimo SharePoint: {}",
            u.host_str().unwrap_or("?")
        )));
    }
    Ok(u)
}

/// Stáhne soubor ze SharePointu do `dest` (přes dočasný `*.part`); vrátí počet bajtů.
pub async fn download_sharepoint(
    http: &Http,
    graph_base: &str,
    token: &str,
    url: &str,
    dest: &Path,
) -> Result<u64> {
    let resp = http
        .client
        .get(format!("{graph_base}{}", share_content_path(url)))
        .bearer_auth(token)
        .send_logged()
        .await
        .map_err(net_err)?;
    let resp = if resp.status().is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Protocol("přesměrování bez Location".into()))?;
        http.client
            .get(redirect_target(location)?)
            .timeout(DOWNLOAD_TIMEOUT)
            .send_logged()
            .await
            .map_err(net_err)?
    } else {
        resp
    };
    let resp = check(resp).await?;
    write_body(resp, dest).await
}

/// Zapíše tělo odpovědi do souboru po částech.
pub(crate) async fn write_body(mut resp: reqwest::Response, dest: &Path) -> Result<u64> {
    let part = dest.with_extension(match dest.extension() {
        Some(e) => format!("{}.part", e.to_string_lossy()),
        None => "part".into(),
    });
    let io = |e: std::io::Error| Error::Other(format!("zápis {}: {e}", part.display()));
    let mut file = tokio::fs::File::create(&part).await.map_err(io)?;
    let mut total = 0u64;
    let result: Result<()> = async {
        while let Some(chunk) = resp.chunk().await.map_err(net_err)? {
            file.write_all(&chunk).await.map_err(io)?;
            total += chunk.len() as u64;
        }
        file.flush().await.map_err(io)
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest).await.map_err(io)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn encodes_share_id() {
        // příklad z dokumentace Graph
        assert_eq!(
            share_content_path(
                "https://onedrive.live.com/redir?resid=1231244193912!12&authKey=1201919!12921!1"
            ),
            "/v1.0/shares/u!aHR0cHM6Ly9vbmVkcml2ZS5saXZlLmNvbS9yZWRpcj9yZXNpZD0xMjMxMjQ0MTkzOTEyITEyJmF1dGhLZXk9MTIwMTkxOSExMjkyMSEx/driveItem/content"
        );
        assert!(is_sharepoint(
            "https://contoso-my.sharepoint.com/personal/a/Documents/x.pdf"
        ));
        assert!(!is_sharepoint(
            "https://eu-api.asm.skype.com/v1/objects/0-x"
        ));
        assert!(redirect_target("https://evil.example/x").is_err());
        assert!(redirect_target("https://contoso-my.sharepoint.com/download.aspx").is_ok());
    }

    #[tokio::test]
    async fn follows_redirect_without_token() {
        let server = MockServer::start().await;
        let file = "https://contoso-my.sharepoint.com/personal/a/Documents/x.pdf";
        Mock::given(method("GET"))
            .and(path(share_content_path(file).as_str()))
            .and(header("Authorization", "Bearer t"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/dl?tempauth=x", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dl"))
            .respond_with(|req: &wiremock::Request| {
                assert!(
                    req.headers.get("authorization").is_none(),
                    "token mimo Graph"
                );
                ResponseTemplate::new(200).set_body_bytes(b"%PDF-obsah".to_vec())
            })
            .expect(1)
            .mount(&server)
            .await;
        let dir = std::env::temp_dir().join(format!("tymy-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("x.pdf");
        let http = Http::new().unwrap();
        let n = download_sharepoint(&http, &server.uri(), "t", file, &dest)
            .await
            .unwrap();
        assert_eq!(n, 10);
        assert_eq!(std::fs::read(&dest).unwrap(), b"%PDF-obsah");
        assert!(!dir.join("x.pdf.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
