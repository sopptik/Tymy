//! Middle tier Teams (`teams.microsoft.com/api/mt/<region>`): profily a hledání lidí.

use reqwest::Method;
use serde_json::{Value, json};
use tymy_core::{Error, Person, Result, UserId};
use url::Url;

use crate::auth::SkypeSession;
use crate::http::{Http, SendLogged, check, json, net_err};

/// URL na middle tier; `path` je relativní k `…/api/mt/<region>/`.
pub(crate) fn mt_url(session: &SkypeSession, path: &str) -> Result<Url> {
    let base = session
        .middle_tier
        .as_ref()
        .ok_or_else(|| Error::Protocol("authz odpověď neobsahovala middleTier".into()))?;
    let joined = format!(
        "{}/{}",
        base.as_str().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let url = Url::parse(&joined).map_err(|e| Error::Protocol(format!("{joined}: {e}")))?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path().trim_end_matches('/')) {
        return Err(Error::Protocol("cesta míří mimo middle tier".into()));
    }
    Ok(url)
}

/// Obecný požadavek na middle tier (ladění protokolu i základ pro hledání lidí).
pub async fn mt_request(
    http: &Http,
    session: &SkypeSession,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let url = mt_url(session, path)?;
    let mut rb = http
        .client
        .request(method, url)
        .bearer_auth(&session.bearer)
        .header("X-Skypetoken", &session.token);
    rb = match body {
        Some(b) => rb.json(b),
        None => rb.header(reqwest::header::CONTENT_LENGTH, 0),
    };
    let resp = check(rb.send_logged().await.map_err(net_err)?).await?;
    if resp.content_length() == Some(0) {
        return Ok(Value::Null);
    }
    json(resp).await
}

/// Profily podle MRI (`8:orgid:<oid>`), po dávkách. Ověřeno 2026-11-04 (tymy-cli mt).
pub async fn fetch_profiles(
    http: &Http,
    session: &SkypeSession,
    mris: &[&str],
) -> Result<Vec<Person>> {
    let mut out = vec![];
    for chunk in mris.chunks(PROFILE_BATCH) {
        let body = json!(chunk);
        let v = mt_request(
            http,
            session,
            Method::POST,
            "beta/users/fetchShortProfile?isMailAddress=false&enableGuest=true&includeIBBarredUsers=true&skypeTeamsInfo=true",
            Some(&body),
        )
        .await?;
        out.extend(parse_people(&v, false));
    }
    Ok(out)
}

/// Kolik profilů žádat najednou.
const PROFILE_BATCH: usize = 50;

/// Hledání v adresáři organizace (ověřeno 2026-11-04).
pub async fn search_directory(
    http: &Http,
    session: &SkypeSession,
    query: &str,
) -> Result<Vec<Person>> {
    let body = json!(query);
    let v = mt_request(
        http,
        session,
        Method::POST,
        "beta/users/searchV2?includeDLs=false&includeBots=false&enableGuest=true&source=newChat&skypeTeamsInfo=true",
        Some(&body),
    )
    .await?;
    Ok(parse_people(&v, false))
}

/// Hledání mimo organizaci podle e-mailu. NEOVĚŘENO: endpoint zatím vrací 400,
/// zřejmě chybí hlavička nebo token, které posílá web klient.
pub async fn search_external(
    http: &Http,
    session: &SkypeSession,
    email: &str,
) -> Result<Vec<Person>> {
    if !email.contains('@') || email.contains('/') || email.contains('?') {
        return Err(Error::Other("zadejte celou e-mailovou adresu".into()));
    }
    let v = mt_request(
        http,
        session,
        Method::GET,
        &format!("beta/users/{email}/externalsearchv3?includeTFLUsers=true"),
        None,
    )
    .await?;
    Ok(parse_people(&v, true))
}

/// Seznam lidí z odpovědi: pole, nebo objekt s polem `value`.
fn parse_people(v: &Value, external: bool) -> Vec<Person> {
    let items = match v {
        Value::Array(a) => a.as_slice(),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        _ => &[],
    };
    items
        .iter()
        .filter_map(|p| {
            let s = |k: &str| {
                p.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            let mri = s("mri")?;
            if !mri.starts_with("8:") {
                return None; // skupiny, boti apod.
            }
            Some(Person {
                display_name: s("displayName").unwrap_or_else(|| mri.clone()),
                mri: UserId(mri),
                email: s("email")
                    .or_else(|| s("mail"))
                    .or_else(|| s("userPrincipalName")),
                job_title: s("jobTitle"),
                department: s("department"),
                company: s("companyName").or_else(|| s("tenantName")),
                external,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn session(mt: Option<&str>) -> SkypeSession {
        SkypeSession {
            token: "st".into(),
            expires_at: Utc::now() + Duration::hours(1),
            chat_service: "https://emea.ng.msg.teams.microsoft.com".parse().unwrap(),
            bearer: "b".into(),
            middle_tier: mt.map(|u| u.parse().unwrap()),
            services: Default::default(),
        }
    }

    #[test]
    fn parses_search_results() {
        let v = serde_json::json!({
            "value": [
                {"mri": "8:orgid:o1", "displayName": "Jan Novák", "email": "jan@firma.cz", "jobTitle": "Vývojář", "department": "IT"},
                {"mri": "28:bot", "displayName": "Bot"},
                {"displayName": "bez mri"}
            ],
            "bestMatchSuggestions": []
        });
        let people = parse_people(&v, false);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].display_name, "Jan Novák");
        assert_eq!(people[0].object_id(), Some("o1"));
        assert_eq!(people[0].subtitle(), "Vývojář · IT · jan@firma.cz");
        let ext = parse_people(
            &serde_json::json!([{"mri": "8:live:x", "displayName": "X"}]),
            true,
        );
        assert!(ext[0].external);
    }

    #[test]
    fn builds_urls_inside_middle_tier_only() {
        let s = session(Some("https://teams.microsoft.com/api/mt/emea"));
        assert_eq!(
            mt_url(&s, "/beta/users/searchV2?x=1").unwrap().as_str(),
            "https://teams.microsoft.com/api/mt/emea/beta/users/searchV2?x=1"
        );
        assert!(mt_url(&s, "../../../evil").is_err());
        assert!(mt_url(&session(None), "beta").is_err());
    }
}
