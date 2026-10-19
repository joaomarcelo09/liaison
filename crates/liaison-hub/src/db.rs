//! SQLite storage. All access goes through one connection behind a mutex; every query is
//! small, and a single-user desktop hub never needs more.

use anyhow::Result;
use liaison_core::{
    Delivery, Direction, Harness, Message, MessageKind, Question, Session, SessionStatus, now_ms,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::{path::Path, sync::Mutex};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    harness         TEXT NOT NULL,
    external_id     TEXT NOT NULL,
    title           TEXT,
    title_locked    INTEGER NOT NULL DEFAULT 0,
    cwd             TEXT,
    transcript_path TEXT,
    pid             INTEGER,
    pid_start       INTEGER,
    status          TEXT NOT NULL,
    status_text     TEXT,
    listen          INTEGER NOT NULL DEFAULT 0,
    mirror          INTEGER NOT NULL DEFAULT 0,
    archived        INTEGER NOT NULL DEFAULT 0,
    awaiting_reply  INTEGER NOT NULL DEFAULT 0,
    ended_reason    TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    last_seen_at    INTEGER NOT NULL,
    UNIQUE (harness, external_id)
);
CREATE INDEX sessions_pid ON sessions (pid);

CREATE TABLE messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    direction    TEXT NOT NULL,
    kind         TEXT NOT NULL,
    body         TEXT NOT NULL,
    meta         TEXT NOT NULL DEFAULT '{}',
    delivery     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    delivered_at INTEGER,
    read_at      INTEGER
);
CREATE INDEX messages_session ON messages (session_id, id);
CREATE INDEX messages_delivery ON messages (session_id, direction, delivery);

CREATE TABLE questions (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    prompt      TEXT NOT NULL,
    options     TEXT NOT NULL DEFAULT '[]',
    answer      TEXT,
    status      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    answered_at INTEGER,
    expires_at  INTEGER
);
CREATE INDEX questions_message ON questions (message_id);
"#;

/// Columns selected for a `Session`, including the computed counters.
const SESSION_COLUMNS: &str = r#"
    s.id, s.harness, s.external_id, s.title, s.cwd, s.transcript_path, s.pid, s.pid_start,
    s.status, s.status_text, s.listen, s.mirror, s.archived, s.awaiting_reply, s.ended_reason,
    s.created_at, s.updated_at, s.last_seen_at,
    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id AND m.direction = 'from_agent'
        AND m.kind != 'event' AND m.read_at IS NULL) AS unread,
    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id AND m.direction = 'to_agent'
        AND m.delivery = 'queued') AS queued,
    (SELECT m.body FROM messages m WHERE m.session_id = s.id AND m.kind != 'event'
        ORDER BY m.id DESC LIMIT 1) AS last_message,
    (SELECT m.created_at FROM messages m WHERE m.session_id = s.id ORDER BY m.id DESC LIMIT 1) AS last_message_at
"#;

const MESSAGE_COLUMNS: &str =
    "id, session_id, direction, kind, body, meta, delivery, created_at, delivered_at, read_at";

const QUESTION_COLUMNS: &str =
    "id, session_id, message_id, kind, prompt, options, answer, status, created_at, answered_at, expires_at";

pub struct Db {
    conn: Mutex<Connection>,
}

/// Fields of a session the harness reports when it (re)connects.
pub struct Registration<'a> {
    pub harness: Harness,
    pub external_id: &'a str,
    pub cwd: Option<&'a str>,
    pub title: Option<&'a str>,
    pub pid: Option<i64>,
    pub pid_start: Option<i64>,
    pub transcript_path: Option<&'a str>,
    pub status: SessionStatus,
}

fn parse<T: std::str::FromStr<Err = String>>(idx: usize, value: String) -> rusqlite::Result<T> {
    value.parse().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, e.into())
    })
}

fn session_from_row(row: &Row) -> rusqlite::Result<Session> {
    let last_message: Option<String> = row.get(20)?;
    Ok(Session {
        id: row.get(0)?,
        harness: parse(1, row.get(1)?)?,
        external_id: row.get(2)?,
        title: row.get(3)?,
        cwd: row.get(4)?,
        transcript_path: row.get(5)?,
        pid: row.get(6)?,
        pid_start: row.get(7)?,
        status: parse(8, row.get(8)?)?,
        status_text: row.get(9)?,
        listen: row.get(10)?,
        mirror: row.get(11)?,
        archived: row.get(12)?,
        awaiting_reply: row.get(13)?,
        ended_reason: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        last_seen_at: row.get(17)?,
        unread: row.get(18)?,
        queued: row.get(19)?,
        last_message: last_message.map(|m| preview(&m)),
        last_message_at: row.get(21)?,
    })
}

fn message_from_row(row: &Row) -> rusqlite::Result<Message> {
    let meta: String = row.get(5)?;
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        direction: parse(2, row.get(2)?)?,
        kind: parse(3, row.get(3)?)?,
        body: row.get(4)?,
        meta: serde_json::from_str(&meta).unwrap_or(serde_json::Value::Null),
        delivery: parse(6, row.get(6)?)?,
        created_at: row.get(7)?,
        delivered_at: row.get(8)?,
        read_at: row.get(9)?,
        question: None,
    })
}

fn question_from_row(row: &Row) -> rusqlite::Result<Question> {
    let options: String = row.get(5)?;
    Ok(Question {
        id: row.get(0)?,
        session_id: row.get(1)?,
        message_id: row.get(2)?,
        kind: parse(3, row.get(3)?)?,
        prompt: row.get(4)?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        answer: row.get(6)?,
        status: parse(7, row.get(7)?)?,
        created_at: row.get(8)?,
        answered_at: row.get(9)?,
        expires_at: row.get(10)?,
    })
}

/// A one-line preview of a message body.
fn preview(body: &str) -> String {
    let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 140 { format!("{}…", line.chars().take(140).collect::<String>()) } else { line }
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version > SCHEMA_VERSION {
            anyhow::bail!("database {} was created by a newer Liaison (schema {version})", path.display());
        }
        Ok(Db { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&conn)?)
    }

    // ── sessions ────────────────────────────────────────────────────────────

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        self.with(|c| {
            c.query_row(&format!("SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.id = ?1"), [id], session_from_row)
                .optional()
        })
    }

    pub fn session_by_external(&self, harness: Harness, external_id: &str) -> Result<Option<Session>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.harness = ?1 AND s.external_id = ?2"),
                params![harness.as_str(), external_id],
                session_from_row,
            )
            .optional()
        })
    }

    /// The connected session most recently active in the given harness process.
    pub fn session_by_pid(&self, pid: i64) -> Result<Option<Session>> {
        self.with(|c| {
            c.query_row(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.pid = ?1
                     AND s.status IN ('active', 'idle', 'listening')
                     ORDER BY s.last_seen_at DESC LIMIT 1"
                ),
                [pid],
                session_from_row,
            )
            .optional()
        })
    }

    pub fn sessions(&self, include_archived: bool) -> Result<Vec<Session>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s WHERE (?1 OR s.archived = 0)
                 ORDER BY COALESCE(last_message_at, s.updated_at) DESC"
            ))?;
            stmt.query_map([include_archived], session_from_row)?.collect()
        })
    }

    /// Connected sessions; checked by the liveness monitor.
    pub fn connected_sessions(&self) -> Result<Vec<Session>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.status IN ('active', 'idle', 'listening')"
            ))?;
            stmt.query_map([], session_from_row)?.collect()
        })
    }

    pub fn sessions_with_status(&self, status: SessionStatus) -> Result<Vec<Session>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.status = ?1"))?;
            stmt.query_map([status.as_str()], session_from_row)?.collect()
        })
    }

    /// Creates the session, or updates it when the harness already registered it before.
    /// Returns the session and its status before the call (None when new).
    pub fn upsert_session(&self, r: &Registration) -> Result<(Session, Option<SessionStatus>)> {
        let now = now_ms();
        let previous = self.session_by_external(r.harness, r.external_id)?;
        let id = match &previous {
            Some(s) => {
                self.with(|c| {
                    c.execute(
                        "UPDATE sessions SET
                            cwd = COALESCE(?2, cwd),
                            title = CASE WHEN title_locked = 0 AND ?3 IS NOT NULL THEN ?3 ELSE title END,
                            pid = COALESCE(?4, pid),
                            pid_start = CASE WHEN ?4 IS NOT NULL THEN ?5 ELSE pid_start END,
                            transcript_path = COALESCE(?6, transcript_path),
                            status = ?7, ended_reason = NULL, updated_at = ?8, last_seen_at = ?8
                         WHERE id = ?1",
                        params![s.id, r.cwd, r.title, r.pid, r.pid_start, r.transcript_path, r.status.as_str(), now],
                    )
                })?;
                s.id.clone()
            }
            None => {
                let id = ulid::Ulid::generate().to_string();
                self.with(|c| {
                    c.execute(
                        "INSERT INTO sessions (id, harness, external_id, title, cwd, transcript_path, pid,
                            pid_start, status, created_at, updated_at, last_seen_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)",
                        params![
                            id,
                            r.harness.as_str(),
                            r.external_id,
                            r.title,
                            r.cwd,
                            r.transcript_path,
                            r.pid,
                            r.pid_start,
                            r.status.as_str(),
                            now
                        ],
                    )
                })?;
                id
            }
        };
        let session = self.session(&id)?.ok_or_else(|| anyhow::anyhow!("session vanished"))?;
        Ok((session, previous.map(|s| s.status)))
    }

    pub fn set_status(&self, id: &str, status: SessionStatus, ended_reason: Option<&str>) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "UPDATE sessions SET status = ?2, ended_reason = ?3, updated_at = ?4, last_seen_at = ?4 WHERE id = ?1",
                params![id, status.as_str(), ended_reason, now],
            )
        })?;
        Ok(())
    }

    /// Like `set_status`, but only when the session is currently in `from`.
    /// Listening → idle for every session (used at start-up, when no waiter can exist).
    pub fn reset_listening(&self) -> Result<usize> {
        let now = now_ms();
        Ok(self.with(|c| {
            c.execute("UPDATE sessions SET status = 'idle', updated_at = ?1 WHERE status = 'listening'", params![now])
        })?)
    }

    pub fn set_status_if(&self, id: &str, from: SessionStatus, to: SessionStatus) -> Result<bool> {
        let now = now_ms();
        let n = self.with(|c| {
            c.execute(
                "UPDATE sessions SET status = ?3, updated_at = ?4 WHERE id = ?1 AND status = ?2",
                params![id, from.as_str(), to.as_str(), now],
            )
        })?;
        Ok(n > 0)
    }

    pub fn touch(&self, id: &str) -> Result<()> {
        self.with(|c| c.execute("UPDATE sessions SET last_seen_at = ?2 WHERE id = ?1", params![id, now_ms()]))?;
        Ok(())
    }

    pub fn set_awaiting_reply(&self, id: &str, value: bool) -> Result<()> {
        self.with(|c| c.execute("UPDATE sessions SET awaiting_reply = ?2 WHERE id = ?1", params![id, value]))?;
        Ok(())
    }

    pub fn patch_session(
        &self,
        id: &str,
        title: Option<&str>,
        title_from_user: bool,
        status_text: Option<&str>,
        listen: Option<bool>,
        mirror: Option<bool>,
        archived: Option<bool>,
    ) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            if let Some(title) = title {
                let title = Some(title.trim()).filter(|t| !t.is_empty());
                if title_from_user {
                    c.execute(
                        "UPDATE sessions SET title = ?2, title_locked = ?3 WHERE id = ?1",
                        params![id, title, title.is_some()],
                    )?;
                } else {
                    c.execute("UPDATE sessions SET title = ?2 WHERE id = ?1 AND title_locked = 0", params![id, title])?;
                }
            }
            if let Some(text) = status_text {
                let text = Some(text.trim()).filter(|t| !t.is_empty());
                c.execute("UPDATE sessions SET status_text = ?2 WHERE id = ?1", params![id, text])?;
            }
            if let Some(v) = listen {
                c.execute("UPDATE sessions SET listen = ?2 WHERE id = ?1", params![id, v])?;
            }
            if let Some(v) = mirror {
                c.execute("UPDATE sessions SET mirror = ?2 WHERE id = ?1", params![id, v])?;
            }
            if let Some(v) = archived {
                c.execute("UPDATE sessions SET archived = ?2 WHERE id = ?1", params![id, v])?;
            }
            c.execute("UPDATE sessions SET updated_at = ?2 WHERE id = ?1", params![id, now])
        })?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<bool> {
        Ok(self.with(|c| c.execute("DELETE FROM sessions WHERE id = ?1", [id]))? > 0)
    }

    /// Sessions of a harness process that are idle and have messages waiting.
    pub fn pending_for_pid(&self, pid: i64) -> Result<Vec<Session>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions s WHERE s.pid = ?1 AND s.status IN ('idle', 'listening')
                 AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id
                     AND m.direction = 'to_agent' AND m.delivery = 'queued')"
            ))?;
            stmt.query_map([pid], session_from_row)?.collect()
        })
    }

    pub fn count_sessions(&self) -> Result<(i64, i64)> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(status IN ('active', 'idle', 'listening')), 0) FROM sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
    }

    // ── messages ────────────────────────────────────────────────────────────

    pub fn insert_message(
        &self,
        session_id: &str,
        direction: Direction,
        kind: MessageKind,
        body: &str,
        meta: &serde_json::Value,
        delivery: Delivery,
    ) -> Result<Message> {
        let now = now_ms();
        let meta = if meta.is_null() { "{}".to_string() } else { meta.to_string() };
        let delivered_at = (delivery != Delivery::Queued).then_some(now);
        let id = self.with(|c| {
            c.execute(
                "INSERT INTO messages (session_id, direction, kind, body, meta, delivery, created_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![session_id, direction.as_str(), kind.as_str(), body, meta, delivery.as_str(), now, delivered_at],
            )?;
            c.execute("UPDATE sessions SET updated_at = ?2 WHERE id = ?1", params![session_id, now])?;
            Ok(c.last_insert_rowid())
        })?;
        self.message(id)?.ok_or_else(|| anyhow::anyhow!("message vanished"))
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        let msg = self.with(|c| {
            c.query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"), [id], message_from_row)
                .optional()
        })?;
        match msg {
            Some(mut m) => {
                m.question = self.question_for_message(m.id)?;
                Ok(Some(m))
            }
            None => Ok(None),
        }
    }

    /// A page of messages, oldest first. `before` pages backwards from a message id.
    pub fn messages(&self, session_id: &str, before: Option<i64>, limit: i64) -> Result<Vec<Message>> {
        let mut messages = self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE session_id = ?1 AND (?2 IS NULL OR id < ?2)
                 ORDER BY id DESC LIMIT ?3"
            ))?;
            stmt.query_map(params![session_id, before, limit], message_from_row)?.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        messages.reverse();
        let questions = self.with(|c| {
            let mut stmt = c.prepare(&format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE session_id = ?1"))?;
            stmt.query_map([session_id], question_from_row)?.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        for q in questions {
            if let Some(m) = messages.iter_mut().find(|m| m.id == q.message_id) {
                m.question = Some(q);
            }
        }
        Ok(messages)
    }

    /// Marks every queued message for the session delivered and returns them.
    pub fn take_queued(&self, session_id: &str) -> Result<Vec<Message>> {
        let now = now_ms();
        let ids: Vec<i64> = self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let ids = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM messages WHERE session_id = ?1 AND direction = 'to_agent'
                     AND delivery = 'queued' ORDER BY id",
                )?;
                stmt.query_map([session_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?
            };
            for id in &ids {
                tx.execute("UPDATE messages SET delivery = 'delivered', delivered_at = ?2 WHERE id = ?1", params![id, now])?;
            }
            tx.commit()?;
            Ok(ids)
        })?;
        ids.into_iter().filter_map(|id| self.message(id).transpose()).collect()
    }

    pub fn queued(&self, session_id: &str) -> Result<Vec<Message>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE session_id = ?1 AND direction = 'to_agent'
                 AND delivery = 'queued' ORDER BY id"
            ))?;
            stmt.query_map([session_id], message_from_row)?.collect()
        })
    }

    /// Marks queued messages undeliverable (the session's context is gone).
    pub fn fail_queued(&self, session_id: &str) -> Result<Vec<Message>> {
        let queued = self.queued(session_id)?;
        self.with(|c| {
            c.execute(
                "UPDATE messages SET delivery = 'undeliverable' WHERE session_id = ?1 AND direction = 'to_agent'
                 AND delivery = 'queued'",
                [session_id],
            )
        })?;
        queued.iter().filter_map(|m| self.message(m.id).transpose()).collect()
    }

    /// Marks the agent's messages read; returns how many changed.
    pub fn mark_read(&self, session_id: &str) -> Result<usize> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "UPDATE messages SET read_at = ?2, delivery = 'read' WHERE session_id = ?1
                 AND direction = 'from_agent' AND read_at IS NULL",
                params![session_id, now],
            )
        })
    }

    /// Whether the agent wrote to the user itself since the last message it received.
    pub fn agent_spoke_since_last_delivery(&self, session_id: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages WHERE session_id = ?1 AND direction = 'from_agent'
                    AND kind IN ('text', 'reply', 'notify', 'question')
                    AND id > COALESCE((SELECT MAX(id) FROM messages WHERE session_id = ?1
                        AND direction = 'to_agent' AND delivery != 'queued'), 0))",
                [session_id],
                |r| r.get(0),
            )
        })
    }

    // ── questions ───────────────────────────────────────────────────────────

    pub fn insert_question(&self, q: &Question) -> Result<()> {
        self.with(|c| {
            c.execute(
                &format!("INSERT INTO questions ({QUESTION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
                params![
                    q.id,
                    q.session_id,
                    q.message_id,
                    q.kind.as_str(),
                    q.prompt,
                    serde_json::to_string(&q.options).unwrap_or_else(|_| "[]".into()),
                    q.answer,
                    q.status.as_str(),
                    q.created_at,
                    q.answered_at,
                    q.expires_at
                ],
            )
        })?;
        Ok(())
    }

    pub fn question(&self, id: &str) -> Result<Option<Question>> {
        self.with(|c| {
            c.query_row(&format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE id = ?1"), [id], question_from_row)
                .optional()
        })
    }

    fn question_for_message(&self, message_id: i64) -> Result<Option<Question>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE message_id = ?1"),
                [message_id],
                question_from_row,
            )
            .optional()
        })
    }

    /// Records an answer; returns false when the question is no longer pending.
    pub fn answer_question(&self, id: &str, answer: &str) -> Result<bool> {
        let now = now_ms();
        let n = self.with(|c| {
            c.execute(
                "UPDATE questions SET answer = ?2, status = 'answered', answered_at = ?3
                 WHERE id = ?1 AND status = 'pending'",
                params![id, answer, now],
            )
        })?;
        if n > 0 {
            self.with(|c| {
                c.execute(
                    "UPDATE messages SET read_at = COALESCE(read_at, ?2), delivery = 'read'
                     WHERE id = (SELECT message_id FROM questions WHERE id = ?1)",
                    params![id, now],
                )
            })?;
        }
        Ok(n > 0)
    }

    /// Expires pending questions past their deadline, or all of a session's when given.
    pub fn expire_questions(&self, session_id: Option<&str>) -> Result<Vec<Question>> {
        let now = now_ms();
        let ids: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM questions WHERE status = 'pending'
                 AND ((?1 IS NOT NULL AND session_id = ?1) OR (?1 IS NULL AND expires_at IS NOT NULL AND expires_at < ?2))",
            )?;
            stmt.query_map(params![session_id, now], |r| r.get(0))?.collect()
        })?;
        for id in &ids {
            self.with(|c| c.execute("UPDATE questions SET status = 'expired' WHERE id = ?1", [id]))?;
        }
        ids.iter().filter_map(|id| self.question(id).transpose()).collect()
    }
}
