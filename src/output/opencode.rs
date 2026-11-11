//! Read-only OpenCode v1 projections, advanced by its atomic per-session event log.
use super::{Export, Info, Message, Part, SessionSnapshot};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, limits::Limit};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_EVENTS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
#[error("OpenCode database history format is incompatible")]
struct IncompatibleDatabase;

struct CachedMessage {
    created: i64,
    bytes: usize,
    message: Message,
}

#[derive(Default)]
struct Cache {
    cursor: Option<(i64, String)>,
    messages: BTreeMap<String, CachedMessage>,
    bytes: usize,
    snapshot: SessionSnapshot,
}

#[derive(Default)]
pub struct Reader {
    sessions: BTreeMap<(PathBuf, String), Cache>,
    cli_sessions: HashSet<(PathBuf, String)>,
}

pub struct Read {
    pub snapshot: SessionSnapshot,
    pub rebuilt: bool,
    pub events_read: usize,
    pub messages_read: usize,
    pub parts_read: usize,
}

impl Reader {
    pub(crate) fn retain(&mut self, sessions: &HashSet<String>) {
        self.sessions
            .retain(|(_, session), _| sessions.contains(session));
        self.cli_sessions
            .retain(|(_, session)| sessions.contains(session));
    }

    /// None selects CLI export only after proven projection incompatibility.
    pub(crate) fn read_compatible(&mut self, path: &Path, session: &str) -> Result<Option<Read>> {
        let key = (path.to_owned(), session.to_owned());
        if self.cli_sessions.contains(&key) {
            return Ok(None);
        }
        match self.read(path, session) {
            Ok(read) => Ok(Some(read)),
            Err(error) if error.is::<IncompatibleDatabase>() => {
                self.sessions.remove(&key);
                self.cli_sessions.insert(key);
                eprintln!("OpenCode database history is incompatible; using CLI export.");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn read(&mut self, path: &Path, session: &str) -> Result<Read> {
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_millis(100))?;
        db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 16 * 1024 * 1024)?;
        db.execute_batch("PRAGMA query_only=ON")?;
        let tx = db.transaction()?;
        let mut update_columns = (false, false);
        for (table, required) in [
            ("session", &["id"][..]),
            ("event", &["id", "aggregate_id", "seq", "type", "data"][..]),
            ("message", &["id", "session_id", "time_created", "data"][..]),
            ("part", &["id", "session_id", "message_id", "data"][..]),
        ] {
            let mut statement = tx.prepare("SELECT name FROM pragma_table_info(?1)")?;
            let columns = statement
                .query_map([table], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            if required.iter().any(|column| !columns.contains(*column)) {
                return Err(IncompatibleDatabase.into());
            }
            match table {
                "message" => update_columns.0 = columns.contains("time_updated"),
                "part" => update_columns.1 = columns.contains("time_updated"),
                _ => {}
            }
        }
        ensure!(
            tx.query_row("SELECT 1 FROM session WHERE id=?1", [session], |_| Ok(()))
                .optional()?
                .is_some(),
            "OpenCode session unavailable in configured database"
        );
        let latest: Option<(i64, String)> = tx
            .query_row(
                "SELECT seq,id FROM event WHERE aggregate_id=?1 ORDER BY seq DESC LIMIT 1",
                [session],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // This reader requires the durable v1 event log. Do not infer change cursors
        // from message IDs or wall-clock timestamps, which miss in-place updates.
        if latest.is_none() {
            return Err(IncompatibleDatabase.into());
        }
        let key = (path.to_owned(), session.to_owned());
        let cached = self.sessions.get(&key);
        let cursor = cached.and_then(|cache| cache.cursor.as_ref());
        let valid = if let Some((seq, id)) = cursor {
            tx.query_row(
                "SELECT id FROM event WHERE aggregate_id=?1 AND seq=?2",
                (session, seq),
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .as_ref()
                == Some(id)
        } else {
            false
        };
        let mut rebuilt = !valid;
        let mut events_read = 0;
        let mut changed = BTreeSet::new();
        if valid && cursor != latest.as_ref() {
            let after = cursor.unwrap().0;
            let mut statement = tx.prepare(
                "SELECT seq,type,CASE type
                 WHEN 'message.updated.1' THEN json_extract(data,'$.info.id')
                 WHEN 'message.part.updated.1' THEN json_extract(data,'$.part.messageID')
                 WHEN 'message.removed.1' THEN json_extract(data,'$.messageID')
                 WHEN 'message.part.removed.1' THEN json_extract(data,'$.messageID') END
                 FROM event WHERE aggregate_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )?;
            let mut rows = statement.query((session, after, (MAX_EVENTS + 1) as i64))?;
            let mut expected = after + 1;
            while let Some(row) = rows.next()? {
                events_read += 1;
                if events_read > MAX_EVENTS || row.get::<_, i64>(0)? != expected {
                    rebuilt = true;
                    break;
                }
                expected += 1;
                let kind: String = row.get(1)?;
                if kind.starts_with("message.") {
                    if !matches!(
                        kind.as_str(),
                        "message.updated.1"
                            | "message.part.updated.1"
                            | "message.removed.1"
                            | "message.part.removed.1"
                    ) {
                        return Err(IncompatibleDatabase.into());
                    }
                    let id: String = row.get(2)?;
                    ensure!(!id.is_empty(), "OpenCode change has no message identity");
                    changed.insert(id);
                }
            }
            if !rebuilt {
                ensure!(
                    expected - 1 == latest.as_ref().unwrap().0,
                    "OpenCode event cursor did not reach its snapshot"
                );
            }
        }
        if rebuilt {
            let unsupported = tx.query_row(
                "SELECT 1 FROM event WHERE aggregate_id=?1 AND type GLOB 'message.*'
                 AND type NOT IN ('message.updated.1','message.part.updated.1','message.removed.1','message.part.removed.1') LIMIT 1",
                [session], |_| Ok(())
            ).optional()?.is_some();
            if unsupported {
                return Err(IncompatibleDatabase.into());
            }
            let mut statement =
                tx.prepare("SELECT id FROM message WHERE session_id=?1 ORDER BY time_created,id")?;
            changed = statement
                .query_map([session], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
        }
        let mut updates = Vec::new();
        let mut parts_read = 0;
        let mut bytes = if rebuilt {
            0
        } else {
            cached.map_or(0, |cache| cache.bytes)
        };
        for id in &changed {
            let update = read_message(&tx, session, id, &mut parts_read, update_columns)?;
            if !rebuilt {
                bytes -= cached
                    .and_then(|cache| cache.messages.get(id))
                    .map_or(0, |message| message.bytes);
            }
            bytes += update.as_ref().map_or(0, |message| message.bytes);
            ensure!(
                bytes <= MAX_BYTES,
                "OpenCode session exceeds 128 MiB of selected content"
            );
            updates.push((id.clone(), update));
        }
        tx.commit()?;
        // A failed/locked/malformed read never advances the in-memory checkpoint.
        let cache = self.sessions.entry(key).or_default();
        if rebuilt {
            *cache = Cache::default();
        }
        cache.bytes = bytes;
        for (id, update) in updates {
            if let Some(message) = update {
                cache.messages.insert(id, message);
            } else {
                cache.messages.remove(&id);
            }
        }
        if rebuilt || !changed.is_empty() {
            let mut messages: Vec<_> = cache.messages.values().collect();
            messages.sort_by(|a, b| {
                (a.created, &a.message.info.id).cmp(&(b.created, &b.message.info.id))
            });
            let export = Export {
                messages: messages
                    .into_iter()
                    .map(|entry| entry.message.clone())
                    .collect(),
            };
            cache.snapshot = SessionSnapshot {
                history: std::sync::Arc::new(super::history_from_data(&export)),
                tasks: super::tasks::from_data(&export),
                decision: super::decision::from_data(&export).ok().flatten(),
                permission: None,
            };
        }
        cache.cursor = latest;
        Ok(Read {
            snapshot: cache.snapshot.clone(),
            rebuilt,
            events_read,
            messages_read: changed.len(),
            parts_read,
        })
    }
}

fn read_message(
    tx: &Transaction<'_>,
    session: &str,
    id: &str,
    parts_read: &mut usize,
    update_columns: (bool, bool),
) -> Result<Option<CachedMessage>> {
    // Optional clock columns preserve compatibility with older projections. Read
    // aggregate part clocks without loading tool output or attachment bodies.
    let updated = if update_columns.0 {
        "time_updated"
    } else {
        "NULL"
    };
    let part_updated = if update_columns.1 {
        "(SELECT MAX(p.time_updated) FROM part p WHERE p.session_id=?1 AND p.message_id=?2)"
    } else {
        "NULL"
    };
    let row: Option<(i64, String, Option<i64>, Option<i64>)> = tx
        .query_row(
            &format!("SELECT time_created,data,{updated},{part_updated} FROM message WHERE session_id=?1 AND id=?2"),
            (session, id),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((created, data, updated, part_updated)) = row else {
        return Ok(None);
    };
    let mut bytes = data.len();
    let mut info: Info = serde_json::from_str(&data)?;
    info.id = id.into();
    info.time.created = info.time.created.or_else(|| u64::try_from(created).ok());
    info.time.updated = info
        .time
        .updated
        .into_iter()
        .chain(
            updated
                .into_iter()
                .chain(part_updated)
                .filter_map(|time| u64::try_from(time).ok()),
        )
        .max();
    let mut parts = Vec::new();
    if info.role == "assistant" || info.role == "user" {
        let mut statement = tx.prepare(
            "SELECT id,data FROM part WHERE session_id=?1 AND message_id=?2 AND
              (json_extract(data,'$.type') IN ('text','file','image') OR
              (json_extract(data,'$.type')='tool' AND json_extract(data,'$.tool') IN ('todowrite','question')))
             ORDER BY id")?;
        let mut rows = statement.query((session, id))?;
        while let Some(row) = rows.next()? {
            let data: String = row.get(1)?;
            bytes += data.len();
            ensure!(
                bytes <= MAX_BYTES,
                "OpenCode message exceeds selected-content limit"
            );
            let mut part: Part = serde_json::from_str(&data)?;
            part.id = Some(row.get(0)?);
            parts.push(part);
            *parts_read += 1;
        }
    }
    Ok(Some(CachedMessage {
        created,
        bytes,
        message: Message { info, parts },
    }))
}
