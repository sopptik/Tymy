#!/usr/bin/env python3
"""Očistí HAR záznam z web klienta Teams od tokenů a osobních údajů a vypíše přehled.

Použití:
    python3 -I tools/har-redact.py ~/teams.har            # zapíše ~/teams.redacted.har + přehled
    python3 -I tools/har-redact.py ~/teams.har -o out.har  # jiný výstup
    python3 -I tools/har-redact.py out.har --summary-only  # jen přehled (už očištěného) záznamu

Odstraní: hlavičky s tokeny a cookies, tokeny v URL, v tělech a ve WebSocket rámcích (JWT,
skypetoken, refresh/access/id token, kód přihlášení, PKCE), e-mailové adresy, binární obsah
odpovědí (obrázky, fonty, skripty). Ponechá strukturu požadavků, která je potřeba k analýze.
Původní soubor nemění. Výsledek je i tak vhodné před sdílením prohlédnout.
"""

import argparse
import base64
import collections
import json
import re
import sys
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

REDACTED = "<redacted>"

SECRET_HEADERS = re.compile(
    r"^(authorization|authentication|cookie|set-cookie|x-skypetoken|x-ms-.*token.*|"
    r".*-token|x-ms-client-request-id-signature|x-ms-session-key|proxy-authorization)$",
    re.I,
)
SECRET_KEYS = re.compile(
    r"^(access_?token|refresh_?token|id_?token|skype_?token|skypetoken|token|tokens|authorization|authentication|x-skypetoken|"
    r"code|client_info|code_verifier|code_challenge|assertion|client_secret|password|"
    r"state|nonce|session_state|sessionId|login_hint|username|upn|email|mail|"
    r"userPrincipalName|sip|primarySmtpAddress|ests.*|.*Cookie.*)$",
    re.I,
)
JWT = re.compile(r"eyJ[\w-]{8,}\.[\w-]{8,}\.[\w-]*")
SKYPETOKEN = re.compile(r"(skypetoken=|skype_token |Bearer |\"skypeToken\"\s*:\s*\")[^\s\"';&]+", re.I)
LONG_SECRET = re.compile(r"\b(?:0\.A[\w-]{40,}|M\.C\d+_[\w!*$.-]{40,}|1\.A[\w-]{40,})")
EMAIL = re.compile(r"[\w.%+-]+@[\w-]+(?:\.[\w-]+)*\.[a-z]{2,}", re.I)
# Osobní obsah (texty zpráv, jména, názvy chatů): k analýze protokolu stačí délka.
TEXT_KEYS = re.compile(
    r"^(content|imdisplayname|displayName|givenName|surname|topic|threadtopic|"
    r"spaceThreadTopic|topicThreadTopic|preview|jobTitle|department|phone.*|mobile.*|"
    r"officeLocation|text|subject|body|bodyPreview|fileName|filename|title)$",
    re.I,
)
KEEP_TEXT = False
SOCKETIO = re.compile(r"^(\d+:[\d+]*:[^:]*:)(.*)$", re.S)
BINARY_MIME = re.compile(r"^(image|font|audio|video)/|javascript|wasm|octet-stream|css", re.I)


def jwt_claims(token: str) -> str:
    """Nahradí JWT jen neutajnými údaji o tom, pro koho a s čím je vydaný (bez podpisu)."""
    try:
        part = token.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except (IndexError, ValueError):
        return REDACTED
    keep = [f"{k}={claims[k]}" for k in ("aud", "scp", "roles", "appid", "azp", "ver") if k in claims]
    return "<jwt " + " ".join(str(x) for x in keep) + ">" if keep else REDACTED


def secret(v) -> str:
    """Hodnota tajné hlavičky nebo klíče: z JWT jen údaje o vydání, jinak úplně pryč."""
    found = JWT.findall(v) if isinstance(v, str) else []
    return " ".join(jwt_claims(t) for t in found) if found else REDACTED


def scrub_text(s: str) -> str:
    s = JWT.sub(lambda m: jwt_claims(m.group(0)), s)
    s = SKYPETOKEN.sub(lambda m: m.group(1) + REDACTED, s)
    s = LONG_SECRET.sub(REDACTED, s)
    return EMAIL.sub("<e-mail>", s)


def scrub_value(k, x):
    if SECRET_KEYS.match(k) and isinstance(x, (str, int)):
        return secret(x)
    if not KEEP_TEXT and TEXT_KEYS.match(k) and isinstance(x, str) and x and not looks_json(x):
        return f"<text {len(x)} znaků>"
    return scrub_json(x)


def looks_json(x: str) -> bool:
    return x.lstrip()[:1] in ("{", "[")


def scrub_json(v):
    if isinstance(v, dict):
        return {k: scrub_value(k, x) for k, x in v.items()}
    if isinstance(v, list):
        return [scrub_json(x) for x in v]
    if isinstance(v, str):
        # JSON vnořený jako text (těla zpráv trouteru)
        if looks_json(v):
            try:
                return json.dumps(scrub_json(json.loads(v)), ensure_ascii=False)
            except ValueError:
                pass
        return scrub_text(v)
    return v


def scrub_body(text: str, mime: str = "") -> str:
    """Tělo: JSON po klíčích, formulář po parametrech, jinak regulární výrazy."""
    m = SOCKETIO.match(text)
    if m and looks_json(m.group(2)):
        # socket.io rámec trouteru: `3:::{…}`, `5:1::{…}`
        return m.group(1) + scrub_body(m.group(2))
    stripped = text.lstrip()
    if stripped[:1] in "{[":
        try:
            return json.dumps(scrub_json(json.loads(text)), ensure_ascii=False)
        except ValueError:
            pass
    if "x-www-form-urlencoded" in mime or (
        "=" in text and "&" in text and " " not in text and not stripped.startswith("<")
    ):
        pairs = parse_qsl(text, keep_blank_values=True)
        if pairs:
            return urlencode(
                [(k, REDACTED if SECRET_KEYS.match(k) else scrub_text(v)) for k, v in pairs]
            )
    return scrub_text(text)


def scrub_url(url: str) -> str:
    parts = urlsplit(url)
    query = urlencode(
        [
            (k, REDACTED if SECRET_KEYS.match(k) else scrub_text(v))
            for k, v in parse_qsl(parts.query, keep_blank_values=True)
        ]
    )
    fragment = scrub_body(parts.fragment) if parts.fragment else ""
    return urlunsplit(parts._replace(path=scrub_text(parts.path), query=query, fragment=fragment))


def scrub_headers(headers):
    return [
        {
            **h,
            "value": secret(h.get("value", ""))
            if SECRET_HEADERS.match(h.get("name", ""))
            else scrub_text(h.get("value", "")),
        }
        for h in headers or []
    ]


def scrub_entry(e):
    req, resp = e.get("request", {}), e.get("response", {})
    req["url"] = scrub_url(req.get("url", ""))
    req["headers"] = scrub_headers(req.get("headers"))
    req["cookies"] = []
    req["queryString"] = [
        {**q, "value": REDACTED if SECRET_KEYS.match(q.get("name", "")) else scrub_text(q.get("value", ""))}
        for q in req.get("queryString", [])
    ]
    post = req.get("postData")
    if post:
        if "text" in post:
            post["text"] = scrub_body(post["text"], post.get("mimeType", ""))
        post["params"] = [
            {**p, "value": REDACTED if SECRET_KEYS.match(p.get("name", "")) else scrub_text(p.get("value", ""))}
            for p in post.get("params", [])
        ]
    resp["headers"] = scrub_headers(resp.get("headers"))
    resp["cookies"] = []
    if "redirectURL" in resp:
        resp["redirectURL"] = scrub_url(resp["redirectURL"])
    content = resp.get("content", {})
    mime = content.get("mimeType", "")
    if "text" in content:
        if BINARY_MIME.search(mime):
            content["text"] = f"<{len(content['text'])} znaků vynecháno>"
            content.pop("encoding", None)
        else:
            text = content["text"]
            if content.get("encoding") == "base64":
                try:
                    text = base64.b64decode(text).decode("utf-8")
                    content.pop("encoding")
                except (ValueError, UnicodeDecodeError):
                    text = "<binární obsah vynechán>"
                    content.pop("encoding")
            content["text"] = scrub_body(text, mime)
    for frame in e.get("_webSocketMessages", []) or []:
        if isinstance(frame.get("data"), str):
            frame["data"] = scrub_body(frame["data"])
    return e


ID_PATTERNS = [
    (re.compile(r"19:[^/?]+@(thread\.(v2|tacv2|skype)|unq\.gbl\.spaces|meeting[^/?]*)"), "{conv}"),
    (re.compile(r"19:meeting_[^/?]+"), "{conv}"),
    (re.compile(r"\d+:(orgid|live|teamsvisitor):[^/?]+"), "{mri}"),
    (re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.I), "{uuid}"),
    (re.compile(r"0-[a-z]+-[a-z0-9]+-[0-9a-f]{32}"), "{ams}"),
    (re.compile(r"(?<=/)\d{6,}(?=/|$)"), "{n}"),
]


def template(path: str) -> str:
    for rx, rep in ID_PATTERNS:
        path = rx.sub(rep, path)
    return path


def summary(har, out=sys.stdout):
    entries = har.get("log", {}).get("entries", [])
    calls = collections.Counter()
    statuses = collections.defaultdict(collections.Counter)
    sockets = []
    for e in entries:
        req = e.get("request", {})
        u = urlsplit(req.get("url", ""))
        if u.scheme in ("ws", "wss") or e.get("_webSocketMessages"):
            sockets.append(e)
            continue
        if BINARY_MIME.search(e.get("response", {}).get("content", {}).get("mimeType", "")):
            continue
        key = (req.get("method", "?"), u.netloc, template(u.path))
        calls[key] += 1
        statuses[key][e.get("response", {}).get("status", 0)] += 1
    print(f"Požadavků celkem: {len(entries)}, API volání (bez statických souborů):", file=out)
    for (method, host, path), n in sorted(calls.items(), key=lambda kv: (kv[0][1], kv[0][2])):
        st = ",".join(f"{s}" + (f"×{c}" if c > 1 else "") for s, c in sorted(statuses[(method, host, path)].items()))
        print(f"  {n:4} {method:6} {host}{path}  [{st}]", file=out)
    print(f"\nWebSockety: {len(sockets)}", file=out)
    for e in sockets:
        u = urlsplit(e["request"]["url"])
        frames = e.get("_webSocketMessages", []) or []
        kinds = collections.Counter(f.get("type") for f in frames)
        print(f"  {u.netloc}{template(u.path)}  rámců: {len(frames)} {dict(kinds)}", file=out)
        prefixes = collections.Counter(re.sub(r"\d+", "#", (f.get("data") or "")[:12]) for f in frames)
        for p, n in prefixes.most_common(8):
            print(f"      {n:4}× začátek {p!r}", file=out)
    if not any(e.get("_webSocketMessages") for e in sockets):
        print("  (žádné rámce: Firefox je do HAR neukládá, použijte Chromium/Chrome)", file=out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("har", type=Path)
    ap.add_argument("-o", "--output", type=Path)
    ap.add_argument("--summary-only", action="store_true", help="jen přehled, nic nezapisovat")
    ap.add_argument("--keep-text", action="store_true", help="ponechat texty zpráv a jména")
    args = ap.parse_args()
    global KEEP_TEXT
    KEEP_TEXT = args.keep_text
    har = json.loads(args.har.read_text(encoding="utf-8"))
    if not args.summary_only:
        for e in har.get("log", {}).get("entries", []):
            scrub_entry(e)
        har.get("log", {}).pop("pages", None)
        out = args.output or args.har.with_name(args.har.stem + ".redacted.har")
        if out.resolve() == args.har.resolve():
            sys.exit("výstup nesmí přepsat původní záznam")
        out.write_text(json.dumps(har, ensure_ascii=False, indent=1), encoding="utf-8")
        print(f"Očištěno: {out}\n")
    summary(har)


if __name__ == "__main__":
    main()
