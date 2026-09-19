//! 术语库 SQLite 内部实现：建表、迁移与行级读写。

use super::matching::{matches_text, normalize};
use super::{Term, TermPolicy, TermStatus};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

/// 当前 schema 版本；结构变化时提升版本号并追加迁移步骤。
const SCHEMA_VERSION: i64 = 1;

/// 建表并执行兼容旧数据的迁移；已是最新版本时直接返回。
pub(super) fn initialize_schema(connection: &mut Connection) -> Result<(), String> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("failed to read terms schema version: {error}"))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("failed to start terms schema migration: {error}"))?;
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS terms (
                 source TEXT PRIMARY KEY,
                 target TEXT NOT NULL,
                 reading TEXT,
                 term_type TEXT NOT NULL,
                 gender TEXT,
                 aliases TEXT NOT NULL DEFAULT '[]',
                 first_chapter INTEGER NOT NULL,
                 note TEXT,
                 status TEXT NOT NULL CHECK (status IN ('ok', 'conflict', 'resolved'))
             );
             CREATE TABLE IF NOT EXISTS term_candidates (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 target TEXT NOT NULL,
                 chapter INTEGER NOT NULL,
                 UNIQUE(source, target)
             );
             CREATE TABLE IF NOT EXISTS pending_term_extractions (
                 chapter_id TEXT NOT NULL,
                 batch_key TEXT NOT NULL,
                 source_text TEXT NOT NULL,
                 target_text TEXT NOT NULL,
                 PRIMARY KEY(chapter_id, batch_key)
             );
             CREATE TABLE IF NOT EXISTS term_aliases (
                 alias_normalized TEXT NOT NULL,
                 alias TEXT NOT NULL,
                 source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 PRIMARY KEY(alias_normalized, source)
             );
             CREATE TABLE IF NOT EXISTS alias_conflicts (
                 alias TEXT NOT NULL,
                 first_source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 second_source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 UNIQUE(alias, first_source, second_source)
             );
             CREATE TABLE IF NOT EXISTS term_rules (
                 source TEXT PRIMARY KEY REFERENCES terms(source) ON DELETE CASCADE,
                 policy TEXT NOT NULL CHECK (policy IN ('automatic', 'fixed', 'non_fixed', 'ignored')),
                 manual_target TEXT,
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE IF NOT EXISTS term_evidence (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 target TEXT NOT NULL,
                 chapter INTEGER NOT NULL,
                 source_excerpt TEXT NOT NULL DEFAULT '',
                 target_excerpt TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS term_conflicts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source TEXT NOT NULL REFERENCES terms(source) ON DELETE CASCADE,
                 target TEXT NOT NULL,
                 resolved INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 resolved_at TEXT,
                 UNIQUE(source, target)
             );
             CREATE TABLE IF NOT EXISTS term_audit (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source TEXT NOT NULL,
                 action TEXT NOT NULL,
                 details TEXT NOT NULL DEFAULT '{}',
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .map_err(|error| format!("failed to initialize terms database: {error}"))?;
    // 把旧版本散落在 terms/term_candidates 中的数据补进规则、证据与冲突表。
    transaction
        .execute_batch(
            "INSERT OR IGNORE INTO term_rules (source, policy, manual_target)
         SELECT source, 'fixed', target FROM terms WHERE status = 'resolved';
         INSERT INTO term_evidence (source, target, chapter)
         SELECT t.source, t.target, t.first_chapter FROM terms t
         WHERE NOT EXISTS (SELECT 1 FROM term_evidence e WHERE e.source = t.source);
         INSERT INTO term_evidence (source, target, chapter)
         SELECT c.source, c.target, c.chapter FROM term_candidates c
         WHERE NOT EXISTS (
             SELECT 1 FROM term_evidence e
             WHERE e.source = c.source AND e.target = c.target AND e.chapter = c.chapter
         );
         INSERT OR IGNORE INTO term_conflicts (source, target, resolved)
         SELECT c.source, c.target, 0 FROM term_candidates c
         JOIN terms t ON t.source = c.source
         WHERE t.status = 'conflict' AND c.target != t.target;
         INSERT OR IGNORE INTO term_conflicts (source, target, resolved, resolved_at)
         SELECT c.source, c.target, 1, CURRENT_TIMESTAMP FROM term_candidates c
         JOIN terms t ON t.source = c.source
         WHERE t.status = 'resolved' AND c.target != t.target;
         UPDATE terms SET status = 'ok'
         WHERE status = 'conflict' AND NOT EXISTS (
             SELECT 1 FROM term_conflicts c WHERE c.source = terms.source AND c.resolved = 0
         );",
        )
        .map_err(|error| format!("failed to migrate terms database: {error}"))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("failed to record terms schema version: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("failed to commit terms schema migration: {error}"))
}

pub(super) fn read_policy(connection: &Connection, source: &str) -> Result<TermPolicy, String> {
    read_rule(connection, source).map(|value| value.0)
}

pub(super) fn read_rule(
    connection: &Connection,
    source: &str,
) -> Result<(TermPolicy, Option<String>), String> {
    connection
        .query_row(
            "SELECT policy, manual_target FROM term_rules WHERE source = ?1",
            [source],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("failed to read term rule: {error}"))?
        .map(|(policy, target)| TermPolicy::parse(&policy).map(|policy| (policy, target)))
        .transpose()
        .map(|value| value.unwrap_or((TermPolicy::Automatic, None)))
}

pub(super) fn write_rule(
    transaction: &Transaction<'_>,
    source: &str,
    policy: TermPolicy,
    manual_target: Option<&str>,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO term_rules (source, policy, manual_target, updated_at)
         VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
         ON CONFLICT(source) DO UPDATE SET policy = excluded.policy,
         manual_target = excluded.manual_target, updated_at = CURRENT_TIMESTAMP",
            params![source, policy.as_str(), manual_target],
        )
        .map_err(|error| format!("failed to save term rule: {error}"))?;
    Ok(())
}

pub(super) fn record_evidence(
    transaction: &Transaction<'_>,
    term: &Term,
    source_text: &str,
    target_text: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO term_evidence
         (source, target, chapter, source_excerpt, target_excerpt)
         VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                term.source,
                term.target,
                term.first_chapter,
                excerpt(source_text, &term.source),
                excerpt(target_text, &term.target),
            ],
        )
        .map_err(|error| format!("failed to record term evidence: {error}"))?;
    Ok(())
}

pub(super) fn record_conflict(
    transaction: &Transaction<'_>,
    source: &str,
    target: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO term_conflicts (source, target) VALUES (?1, ?2)",
            params![source, target],
        )
        .map_err(|error| format!("failed to record term conflict: {error}"))?;
    Ok(())
}

pub(super) fn audit(
    transaction: &Transaction<'_>,
    source: &str,
    action: &str,
    details: serde_json::Value,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO term_audit (source, action, details) VALUES (?1, ?2, ?3)",
            params![source, action, details.to_string()],
        )
        .map_err(|error| format!("failed to record term audit: {error}"))?;
    Ok(())
}

/// 在文本中截取包含关键词的行作为证据，最多 240 字符。
fn excerpt(text: &str, needle: &str) -> String {
    text.lines()
        .find(|line| matches_text(line, needle))
        .unwrap_or_else(|| text.lines().next().unwrap_or_default())
        .chars()
        .take(240)
        .collect()
}

pub(super) fn validate_term(term: &Term) -> Result<(), String> {
    if term.source.trim().is_empty() || term.target.trim().is_empty() {
        return Err("term source and target must not be empty".to_string());
    }
    const TYPES: [&str; 7] = [
        "person",
        "place",
        "organization",
        "term",
        "appellation",
        "speech",
        "fixed_expr",
    ];
    if !TYPES.contains(&term.term_type.as_str()) {
        return Err(format!("unsupported term type: {}", term.term_type));
    }
    Ok(())
}

pub(super) fn write_new_term(transaction: &Transaction<'_>, term: &Term) -> Result<(), String> {
    let aliases = serde_json::to_string(&term.aliases)
        .map_err(|error| format!("failed to encode aliases: {error}"))?;
    transaction
        .execute(
            "INSERT INTO terms
             (source, target, reading, term_type, gender, aliases, first_chapter, note, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                term.source,
                term.target,
                term.reading,
                term.term_type,
                term.gender,
                aliases,
                term.first_chapter,
                term.note,
                term.status.as_str(),
            ],
        )
        .map_err(|error| format!("failed to insert term: {error}"))?;
    Ok(())
}

/// 合并重复术语的元数据：别名取并集，其余字段保留已有值，首次出现章节取更早者。
pub(super) fn merge_term_metadata(
    transaction: &Transaction<'_>,
    existing: &Term,
    incoming: &Term,
) -> Result<(), String> {
    let mut aliases = existing.aliases.clone();
    for alias in &incoming.aliases {
        if !aliases.contains(alias) {
            aliases.push(alias.clone());
        }
    }
    let aliases = serde_json::to_string(&aliases)
        .map_err(|error| format!("failed to encode aliases: {error}"))?;
    transaction
        .execute(
            "UPDATE terms SET aliases = ?2,
             reading = COALESCE(reading, ?3), gender = COALESCE(gender, ?4),
             note = COALESCE(note, ?5), first_chapter = MIN(first_chapter, ?6)
             WHERE source = ?1",
            params![
                incoming.source,
                aliases,
                incoming.reading,
                incoming.gender,
                incoming.note,
                incoming.first_chapter,
            ],
        )
        .map_err(|error| format!("failed to merge term metadata: {error}"))?;
    Ok(())
}

pub(super) fn record_candidate(
    transaction: &Transaction<'_>,
    source: &str,
    target: &str,
    chapter: usize,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO term_candidates (source, target, chapter)
             VALUES (?1, ?2, ?3)",
            params![source, target, chapter],
        )
        .map_err(|error| format!("failed to record term candidate: {error}"))?;
    Ok(())
}

/// 同步别名表，并在同一别名被多个术语使用时记录别名冲突。
pub(super) fn sync_aliases(
    transaction: &Transaction<'_>,
    source: &str,
    aliases: &[String],
) -> Result<(), String> {
    for alias in aliases {
        let normalized = normalize(alias);
        if normalized.is_empty() {
            continue;
        }
        let mut statement = transaction
            .prepare(
                "SELECT source FROM term_aliases
                 WHERE alias_normalized = ?1 AND source != ?2",
            )
            .map_err(|error| format!("failed to prepare alias lookup: {error}"))?;
        let others = statement
            .query_map(params![normalized, source], |row| row.get::<_, String>(0))
            .map_err(|error| format!("failed to query alias owners: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to read alias owner: {error}"))?;
        drop(statement);
        transaction
            .execute(
                "INSERT OR IGNORE INTO term_aliases (alias_normalized, alias, source)
                 VALUES (?1, ?2, ?3)",
                params![normalized, alias, source],
            )
            .map_err(|error| format!("failed to store term alias: {error}"))?;
        for other in others {
            let (first, second) = if source < other.as_str() {
                (source, other.as_str())
            } else {
                (other.as_str(), source)
            };
            transaction
                .execute(
                    "INSERT OR IGNORE INTO alias_conflicts
                     (alias, first_source, second_source) VALUES (?1, ?2, ?3)",
                    params![alias, first, second],
                )
                .map_err(|error| format!("failed to record alias conflict: {error}"))?;
        }
    }
    Ok(())
}

pub(super) fn find_term(
    transaction: &Transaction<'_>,
    source: &str,
) -> Result<Option<Term>, String> {
    transaction
        .query_row(
            "SELECT source, target, reading, term_type, gender, aliases,
             first_chapter, note, status FROM terms WHERE source = ?1",
            [source],
            row_to_term,
        )
        .optional()
        .map_err(|error| format!("failed to query term: {error}"))?
        .map(parse_stored_term)
        .transpose()
}

type StoredTerm = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    usize,
    Option<String>,
    String,
);

pub(super) fn row_to_term(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTerm> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

pub(super) fn parse_stored_term(value: StoredTerm) -> Result<Term, String> {
    Ok(Term {
        source: value.0,
        target: value.1,
        reading: value.2,
        term_type: value.3,
        gender: value.4,
        aliases: serde_json::from_str(&value.5)
            .map_err(|error| format!("invalid aliases in terms database: {error}"))?,
        first_chapter: value.6,
        note: value.7,
        status: TermStatus::parse(&value.8)?,
        policy: TermPolicy::Automatic,
        manual_target: None,
    })
}
