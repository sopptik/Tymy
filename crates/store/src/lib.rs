//! Lokální cache konverzací a zpráv v SQLite.
//!
//! Doménové typy ukládáme jako JSON: schéma tak nemusí sledovat každou změnu modelu
//! a při nečitelném záznamu ho prostě přeskočíme (cache, ne zdroj pravdy).
//! Tajemství sem nepatří, ta jsou v Secret Service.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use tymy_core::text::html_to_plain;
use tymy_core::{AccountId, Conversation, ConversationId, Error, Message, Result};

/// Verze schématu; při zvýšení přidat migraci do [`migrate`].
const SCHEMA_VERSION: i64 = 2;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Storage(format!("SQLite: {e}"))
}

fn json_err(e: serde_json::Error) -> Error {
    Error::Storage(format!("serializace: {e}"))
}

impl Store {
    /// Otevře (nebo vytvoří) šifrovanou databázi (SQLCipher, AES-256).
    ///
    /// Klíč patří do Secret Service. Pokud soubor nejde s tímto klíčem otevřít (jiný klíč,
    /// stará nešifrovaná cache), soubor se zahodí a vytvoří znovu: je to jen cache.
    /// Soubor je navíc čitelný jen pro vlastníka (`0600`, adresář `0700`).
    pub fn open(path: &Path, key: &CacheKey) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let io = |p: &Path, e: std::io::Error| Error::Storage(format!("{}: {e}", p.display()));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| io(dir, e))?;
        }
        let conn = match open_keyed(path, key) {
            Ok(c) => c,
            Err(e) if is_not_a_database(&e) => {
                for p in db_files(path) {
                    if p.exists() {
                        std::fs::remove_file(&p).map_err(|e| io(&p, e))?;
                    }
                }
                open_keyed(path, key).map_err(db_err)?
            }
            Err(e) => return Err(db_err(e)),
        };
        let store = Self::init(conn)?;
        for p in db_files(path) {
            if p.exists() {
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| io(&p, e))?;
            }
        }
        Ok(store)
    }

    /// Nešifrovaná databáze v paměti (testy).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .map_err(db_err)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("zámek databáze je poškozený".into()))?;
        f(&mut conn).map_err(db_err)
    }

    /// Nahradí uložený seznam konverzací účtu.
    pub fn save_conversations(&self, account: &AccountId, convs: &[Conversation]) -> Result<()> {
        let rows: Vec<(String, String, Option<i64>)> = convs
            .iter()
            .map(|c| {
                Ok((
                    c.id.0.clone(),
                    serde_json::to_string(c).map_err(json_err)?,
                    c.last_activity.map(|t| t.timestamp_millis()),
                ))
            })
            .collect::<Result<_>>()?;
        self.with(|conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM conversations WHERE account = ?1", [account.as_str()])?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO conversations (account, id, json, last_activity) VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (id, json, last) in &rows {
                    stmt.execute(params![account.as_str(), id, json, last])?;
                }
            }
            tx.commit()
        })
    }

    /// Konverzace účtu od poslední aktivity.
    pub fn load_conversations(&self, account: &AccountId) -> Result<Vec<Conversation>> {
        let rows: Vec<String> = self.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT json FROM conversations WHERE account = ?1 ORDER BY last_activity DESC",
            )?;
            stmt.query_map([account.as_str()], |r| r.get(0))?.collect()
        })?;
        Ok(rows
            .iter()
            .filter_map(|j| serde_json::from_str(j).ok())
            .collect())
    }

    /// Uloží (nebo přepíše) zprávy; konverzaci bere ze zprávy.
    pub fn save_messages(&self, account: &AccountId, messages: &[Message]) -> Result<()> {
        let rows: Vec<(String, String, i64, String, String)> = messages
            .iter()
            .map(|m| {
                Ok((
                    m.conversation_id.0.clone(),
                    m.id.0.clone(),
                    m.id.as_str().parse::<i64>().unwrap_or(0),
                    serde_json::to_string(m).map_err(json_err)?,
                    if m.deleted || !m.is_user_content() {
                        String::new()
                    } else {
                        html_to_plain(&m.content)
                    },
                ))
            })
            .collect::<Result<_>>()?;
        self.with(|conn| {
            let tx = conn.transaction()?;
            {
                let mut upsert = tx.prepare(
                    "INSERT INTO messages (account, conversation_id, id, id_num, json, plain)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (account, conversation_id, id)
                     DO UPDATE SET json = excluded.json, plain = excluded.plain, id_num = excluded.id_num",
                )?;
                for (conv, id, num, json, plain) in &rows {
                    upsert.execute(params![account.as_str(), conv, id, num, json, plain])?;
                }
            }
            tx.commit()
        })
    }

    /// Nejnovějších `limit` zpráv konverzace, vráceno od nejstarší.
    pub fn load_messages(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let rows: Vec<String> = self.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT json FROM messages WHERE account = ?1 AND conversation_id = ?2
                 ORDER BY id_num DESC LIMIT ?3",
            )?;
            stmt.query_map(
                params![account.as_str(), conversation.as_str(), limit as i64],
                |r| r.get(0),
            )?
            .collect()
        })?;
        let mut msgs: Vec<Message> = rows
            .iter()
            .filter_map(|j| serde_json::from_str(j).ok())
            .collect();
        msgs.reverse();
        Ok(msgs)
    }

    /// Kurzor na starší historii. `None` = neznámý, `Some(None)` = začátek historie.
    pub fn older_cursor(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
    ) -> Result<Option<Option<String>>> {
        self.with(|conn| {
            conn.query_row(
                "SELECT older_cursor FROM conversation_state WHERE account = ?1 AND conversation_id = ?2",
                params![account.as_str(), conversation.as_str()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
    }

    pub fn set_older_cursor(
        &self,
        account: &AccountId,
        conversation: &ConversationId,
        cursor: Option<&str>,
    ) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT INTO conversation_state (account, conversation_id, older_cursor) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account, conversation_id) DO UPDATE SET older_cursor = excluded.older_cursor",
                params![account.as_str(), conversation.as_str(), cursor],
            )
            .map(|_| ())
        })
    }

    /// Fulltextové hledání ve zprávách účtu (nejnovější první).
    pub fn search(&self, account: &AccountId, query: &str, limit: usize) -> Result<Vec<Message>> {
        let Some(fts) = fts_query(query) else {
            return Ok(vec![]);
        };
        let rows: Vec<String> = self.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.json FROM messages_fts f JOIN messages m ON m.rowid = f.rowid
                 WHERE messages_fts MATCH ?1 AND m.account = ?2
                 ORDER BY m.id_num DESC LIMIT ?3",
            )?;
            stmt.query_map(params![fts, account.as_str(), limit as i64], |r| r.get(0))?
                .collect()
        })?;
        Ok(rows
            .iter()
            .filter_map(|j| serde_json::from_str(j).ok())
            .collect())
    }

    /// Uloží stažené médium (obrázek) pod jeho URL.
    pub fn save_media(&self, account: &AccountId, url: &str, bytes: &[u8]) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT INTO media (account, url, bytes, size, used_at) VALUES (?1, ?2, ?3, ?4, unixepoch())
                 ON CONFLICT (account, url) DO UPDATE SET bytes = excluded.bytes, size = excluded.size, used_at = excluded.used_at",
                params![account.as_str(), url, bytes, bytes.len() as i64],
            )
            .map(|_| ())
        })
    }

    /// Médium z cache; zároveň ho označí jako naposledy použité.
    pub fn load_media(&self, account: &AccountId, url: &str) -> Result<Option<Vec<u8>>> {
        self.with(|conn| {
            let bytes = conn
                .query_row(
                    "SELECT bytes FROM media WHERE account = ?1 AND url = ?2",
                    params![account.as_str(), url],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .optional()?;
            if bytes.is_some() {
                conn.execute(
                    "UPDATE media SET used_at = unixepoch() WHERE account = ?1 AND url = ?2",
                    params![account.as_str(), url],
                )?;
            }
            Ok(bytes)
        })
    }

    /// Smaže nejdéle nepoužitá média, dokud jejich celková velikost nepřesahuje `max_bytes`.
    pub fn prune_media(&self, max_bytes: u64) -> Result<()> {
        self.with(|conn| {
            let total: i64 =
                conn.query_row("SELECT COALESCE(SUM(size), 0) FROM media", [], |r| r.get(0))?;
            let mut excess = total - max_bytes as i64;
            if excess <= 0 {
                return Ok(());
            }
            let victims: Vec<(i64, i64)> = {
                let mut stmt =
                    conn.prepare("SELECT rowid, size FROM media ORDER BY used_at ASC, rowid ASC")?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let tx = conn.transaction()?;
            for (rowid, size) in victims {
                if excess <= 0 {
                    break;
                }
                tx.execute("DELETE FROM media WHERE rowid = ?1", [rowid])?;
                excess -= size;
            }
            tx.commit()
        })
    }

    /// Smaže všechna data účtu (např. při odhlášení).
    pub fn forget_account(&self, account: &AccountId) -> Result<()> {
        self.with(|conn| {
            let tx = conn.transaction()?;
            for table in ["conversations", "messages", "conversation_state", "media"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE account = ?1"),
                    [account.as_str()],
                )?;
            }
            tx.commit()
        })
    }
}

/// Šifrovací klíč cache (256 bitů).
pub type CacheKey = [u8; 32];

fn open_keyed(path: &Path, key: &CacheKey) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    // Klíč musí být první příkaz; raw klíč (x'…') přeskočí odvozování z hesla.
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))?;
    // Ověření klíče: se špatným klíčem selže čtení hlavičky (SQLITE_NOTADB).
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(conn)
}

fn is_not_a_database(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::NotADatabase)
}

/// Hlavní soubor databáze a jeho WAL/SHM soubory.
fn db_files(path: &Path) -> [std::path::PathBuf; 3] {
    let with = |suffix: &str| {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        std::path::PathBuf::from(p)
    };
    [path.to_owned(), with("-wal"), with("-shm")]
}

/// Uživatelský dotaz → bezpečný FTS5 dotaz: každé slovo jako prefixová fráze.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(db_err)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Storage(format!(
            "databáze je z novější verze aplikace (schéma {version})"
        )));
    }
    if version < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE conversations (
                 account TEXT NOT NULL,
                 id TEXT NOT NULL,
                 json TEXT NOT NULL,
                 last_activity INTEGER,
                 PRIMARY KEY (account, id)
             );
             CREATE TABLE messages (
                 account TEXT NOT NULL,
                 conversation_id TEXT NOT NULL,
                 id TEXT NOT NULL,
                 id_num INTEGER NOT NULL,
                 json TEXT NOT NULL,
                 plain TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (account, conversation_id, id)
             );
             CREATE INDEX messages_by_time ON messages (account, conversation_id, id_num);
             CREATE TABLE conversation_state (
                 account TEXT NOT NULL,
                 conversation_id TEXT NOT NULL,
                 older_cursor TEXT,
                 PRIMARY KEY (account, conversation_id)
             );
             CREATE VIRTUAL TABLE messages_fts USING fts5(
                 plain, content='messages', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts (rowid, plain) VALUES (new.rowid, new.plain);
             END;
             CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                 INSERT INTO messages_fts (messages_fts, rowid, plain) VALUES ('delete', old.rowid, old.plain);
             END;
             CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
                 INSERT INTO messages_fts (messages_fts, rowid, plain) VALUES ('delete', old.rowid, old.plain);
                 INSERT INTO messages_fts (rowid, plain) VALUES (new.rowid, new.plain);
             END;
             PRAGMA user_version = 1;
             COMMIT;",
        )
        .map_err(db_err)?;
    }
    if version < 2 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE media (
                 account TEXT NOT NULL,
                 url TEXT NOT NULL,
                 bytes BLOB NOT NULL,
                 size INTEGER NOT NULL,
                 used_at INTEGER NOT NULL,
                 PRIMARY KEY (account, url)
             );
             PRAGMA user_version = 2;
             COMMIT;",
        )
        .map_err(db_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tymy_core::{ConversationKind, MessageKind};

    fn acc() -> AccountId {
        "acc".into()
    }

    fn msg(conv: &str, id: &str, text: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: conv.into(),
            client_message_id: None,
            sender: None,
            sender_name: Some("Alice".into()),
            kind: MessageKind::RichText,
            content: format!("<p>{text}</p>"),
            composed_at: None,
            arrived_at: None,
            edited_at: None,
            deleted: false,
            attachments: vec![],
            reactions: vec![],
        }
    }

    fn conv(id: &str, ms: i64) -> Conversation {
        Conversation {
            id: id.into(),
            kind: ConversationKind::Group,
            topic: Some(id.into()),
            last_message: None,
            last_activity: Utc.timestamp_millis_opt(ms).single(),
            read_up_to: None,
        }
    }

    #[test]
    fn conversations_roundtrip_sorted_and_replaced() {
        let s = Store::open_in_memory().unwrap();
        s.save_conversations(&acc(), &[conv("a", 1), conv("b", 3), conv("c", 2)])
            .unwrap();
        let ids: Vec<_> = s
            .load_conversations(&acc())
            .unwrap()
            .into_iter()
            .map(|c| c.id.0)
            .collect();
        assert_eq!(ids, ["b", "c", "a"]);
        s.save_conversations(&acc(), &[conv("z", 1)]).unwrap();
        assert_eq!(s.load_conversations(&acc()).unwrap().len(), 1);
        assert!(s.load_conversations(&"jiny".into()).unwrap().is_empty());
    }

    #[test]
    fn messages_upsert_limit_and_order() {
        let s = Store::open_in_memory().unwrap();
        let c: ConversationId = "c1".into();
        s.save_messages(
            &acc(),
            &[
                msg("c1", "100", "a"),
                msg("c1", "1000", "c"),
                msg("c1", "900", "b"),
            ],
        )
        .unwrap();
        s.save_messages(&acc(), &[msg("c1", "100", "upraveno")])
            .unwrap();
        let all = s.load_messages(&acc(), &c, 10).unwrap();
        let ids: Vec<_> = all.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["100", "900", "1000"]);
        assert_eq!(all[0].content, "<p>upraveno</p>");
        let last2: Vec<_> = s
            .load_messages(&acc(), &c, 2)
            .unwrap()
            .into_iter()
            .map(|m| m.id.0)
            .collect();
        assert_eq!(last2, ["900", "1000"]);
    }

    #[test]
    fn cursor_states() {
        let s = Store::open_in_memory().unwrap();
        let c: ConversationId = "c1".into();
        assert_eq!(s.older_cursor(&acc(), &c).unwrap(), None);
        s.set_older_cursor(&acc(), &c, Some("https://x/older"))
            .unwrap();
        assert_eq!(
            s.older_cursor(&acc(), &c).unwrap(),
            Some(Some("https://x/older".into()))
        );
        s.set_older_cursor(&acc(), &c, None).unwrap();
        assert_eq!(s.older_cursor(&acc(), &c).unwrap(), Some(None));
    }

    #[test]
    fn fulltext_ignores_diacritics_and_follows_updates() {
        let s = Store::open_in_memory().unwrap();
        s.save_messages(
            &acc(),
            &[
                msg("c1", "1", "Posílám <b>přílohu</b>"),
                msg("c2", "2", "něco jiného"),
            ],
        )
        .unwrap();
        let hits = s.search(&acc(), "priloh", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id.as_str(), "1");
        s.save_messages(&acc(), &[msg("c1", "1", "změněno")])
            .unwrap();
        assert!(s.search(&acc(), "priloh", 10).unwrap().is_empty());
        assert_eq!(s.search(&acc(), "zmeneno", 10).unwrap().len(), 1);
        // uvozovky v dotazu nerozbijí FTS syntaxi
        assert!(s.search(&acc(), "\"", 10).unwrap().is_empty());
        assert!(s.search(&acc(), "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn media_roundtrip_and_prune_least_recently_used() {
        let s = Store::open_in_memory().unwrap();
        s.save_media(&acc(), "u1", &[1; 100]).unwrap();
        s.save_media(&acc(), "u2", &[2; 100]).unwrap();
        s.save_media(&acc(), "u3", &[3; 100]).unwrap();
        // u1 použít → stane se nejnovějším (used_at má sekundovou přesnost, proto ruční posun)
        s.with(|c| {
            c.execute(
                "UPDATE media SET used_at = used_at - 10 WHERE url IN ('u2', 'u3')",
                [],
            )
        })
        .unwrap();
        s.with(|c| {
            c.execute(
                "UPDATE media SET used_at = used_at - 20 WHERE url = 'u2'",
                [],
            )
        })
        .unwrap();
        assert_eq!(s.load_media(&acc(), "u1").unwrap().unwrap(), vec![1; 100]);
        s.prune_media(150).unwrap();
        assert!(
            s.load_media(&acc(), "u2").unwrap().is_none(),
            "nejstarší pryč"
        );
        assert!(s.load_media(&acc(), "u3").unwrap().is_none());
        assert!(s.load_media(&acc(), "u1").unwrap().is_some());
        assert!(s.load_media(&"jiny".into(), "u1").unwrap().is_none());
        s.forget_account(&acc()).unwrap();
        assert!(s.load_media(&acc(), "u1").unwrap().is_none());
    }

    fn temp_db(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tymy-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cache.sqlite");
        (dir, path)
    }

    #[test]
    fn forget_account_and_reopen_file() {
        let key = [7u8; 32];
        let (dir, path) = temp_db("reopen");
        {
            let s = Store::open(&path, &key).unwrap();
            s.save_messages(&acc(), &[msg("c1", "1", "x")]).unwrap();
        }
        let s = Store::open(&path, &key).unwrap();
        assert_eq!(s.load_messages(&acc(), &"c1".into(), 10).unwrap().len(), 1);
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        s.forget_account(&acc()).unwrap();
        assert!(
            s.load_messages(&acc(), &"c1".into(), 10)
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_is_encrypted_and_wrong_key_recreates() {
        let (dir, path) = temp_db("crypt");
        {
            let s = Store::open(&path, &[1u8; 32]).unwrap();
            s.save_messages(&acc(), &[msg("c1", "1", "TajnaZpravaXYZ")])
                .unwrap();
        }
        for p in db_files(&path).iter().filter(|p| p.exists()) {
            let bytes = std::fs::read(p).unwrap();
            assert!(
                !bytes.windows(14).any(|w| w == b"TajnaZpravaXYZ"),
                "{} obsahuje nešifrovaný text",
                p.display()
            );
            assert!(
                !bytes.starts_with(b"SQLite format 3"),
                "hlavička není šifrovaná"
            );
        }
        // jiný klíč → cache se zahodí a vytvoří prázdná
        let s = Store::open(&path, &[2u8; 32]).unwrap();
        assert!(
            s.load_messages(&acc(), &"c1".into(), 10)
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_plaintext_cache_is_replaced() {
        let (dir, path) = temp_db("plain");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES ('stara data');")
                .unwrap();
        }
        let s = Store::open(&path, &[3u8; 32]).unwrap();
        s.save_messages(&acc(), &[msg("c1", "1", "nova")]).unwrap();
        assert_eq!(s.load_messages(&acc(), &"c1".into(), 10).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
