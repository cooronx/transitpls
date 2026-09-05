use crate::llm::TranslationClient;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Term {
    pub source: String,
    pub target: String,
    pub reading: Option<String>,
    #[serde(rename = "type")]
    pub term_type: String,
    pub gender: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub first_chapter: usize,
    pub note: Option<String>,
    pub status: TermStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TermStatus {
    Ok,
    Conflict,
    Resolved,
}

impl TermStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Conflict => "conflict",
            Self::Resolved => "resolved",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "ok" => Ok(Self::Ok),
            "conflict" => Ok(Self::Conflict),
            "resolved" => Ok(Self::Resolved),
            _ => Err(format!("invalid term status in database: {value}")),
        }
    }
}

impl std::fmt::Display for TermStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermCandidate {
    pub source: String,
    pub target: String,
    pub chapter: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasConflict {
    pub alias: String,
    pub first_source: String,
    pub second_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingExtraction {
    pub chapter_id: String,
    pub batch_key: String,
    pub source_text: String,
    pub target_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionResponse {
    terms: Vec<Term>,
}

#[derive(Debug, Clone)]
pub struct TermStore {
    path: std::path::PathBuf,
}

impl TermStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path).map_err(|error| {
            format!("failed to open terms database {}: {error}", path.display())
        })?;
        initialize_schema(&connection)?;
        Ok(Self { path })
    }

    fn connect(&self) -> Result<Connection, String> {
        Connection::open(&self.path).map_err(|error| {
            format!(
                "failed to open terms database {}: {error}",
                self.path.display()
            )
        })
    }

    pub fn insert(&self, term: &Term) -> Result<TermStatus, String> {
        validate_term(term)?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("failed to start term transaction: {error}"))?;
        let existing = find_term(&transaction, &term.source)?;
        let status = match existing {
            None => {
                write_new_term(&transaction, term)?;
                sync_aliases(&transaction, &term.source, &term.aliases)?;
                TermStatus::Ok
            }
            Some(existing) if existing.target == term.target => {
                merge_term_metadata(&transaction, &existing, term)?;
                let mut aliases = existing.aliases;
                for alias in &term.aliases {
                    if !aliases.contains(alias) {
                        aliases.push(alias.clone());
                    }
                }
                sync_aliases(&transaction, &term.source, &aliases)?;
                existing.status
            }
            Some(existing) => {
                record_candidate(
                    &transaction,
                    &existing.source,
                    &existing.target,
                    existing.first_chapter,
                )?;
                record_candidate(&transaction, &term.source, &term.target, term.first_chapter)?;
                if existing.status != TermStatus::Resolved {
                    transaction
                        .execute(
                            "UPDATE terms SET status = 'conflict' WHERE source = ?1",
                            [&term.source],
                        )
                        .map_err(|error| format!("failed to mark term conflict: {error}"))?;
                    TermStatus::Conflict
                } else {
                    TermStatus::Resolved
                }
            }
        };
        let status = find_term(&transaction, &term.source)?
            .map(|stored| stored.status)
            .unwrap_or(status);
        transaction
            .commit()
            .map_err(|error| format!("failed to commit term transaction: {error}"))?;
        Ok(status)
    }

    pub fn list(&self) -> Result<Vec<Term>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT source, target, reading, term_type, gender, aliases, \
                 first_chapter, note, status FROM terms ORDER BY source",
            )
            .map_err(|error| format!("failed to prepare term list: {error}"))?;
        let rows = statement
            .query_map([], row_to_term)
            .map_err(|error| format!("failed to query terms: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("failed to read term: {error}")))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_stored_term)
            .collect()
    }

    pub fn conflicts(&self) -> Result<Vec<TermCandidate>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT c.source, c.target, c.chapter
                 FROM term_candidates c
                 JOIN terms t ON t.source = c.source
                 WHERE t.status = 'conflict'
                 ORDER BY c.source, c.id",
            )
            .map_err(|error| format!("failed to prepare conflict list: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(TermCandidate {
                    source: row.get(0)?,
                    target: row.get(1)?,
                    chapter: row.get(2)?,
                })
            })
            .map_err(|error| format!("failed to query term conflicts: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("failed to read conflict: {error}")))
            .collect()
    }

    pub fn resolve(&self, source: &str, target: &str) -> Result<(), String> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err("term source and resolved target must not be empty".to_string());
        }
        let connection = self.connect()?;
        let changed = connection
            .execute(
                "UPDATE terms SET target = ?2, status = 'resolved' WHERE source = ?1",
                params![source, target],
            )
            .map_err(|error| format!("failed to resolve term: {error}"))?;
        if changed == 0 {
            return Err(format!("term not found: {source}"));
        }
        Ok(())
    }

    pub fn relevant(&self, source_text: &str) -> Result<Vec<Term>, String> {
        let terms = self.list()?;
        let direct = terms
            .iter()
            .filter(|term| matches_text(source_text, &term.source))
            .map(|term| term.source.clone())
            .collect::<HashSet<_>>();
        let mut aliases = HashMap::<String, Vec<usize>>::new();
        for (index, term) in terms.iter().enumerate() {
            for alias in &term.aliases {
                if matches_text(source_text, alias) {
                    aliases.entry(normalize(alias)).or_default().push(index);
                }
            }
        }
        let unambiguous = aliases
            .values()
            .filter(|matches| matches.len() == 1)
            .map(|matches| matches[0])
            .collect::<HashSet<_>>();
        Ok(terms
            .into_iter()
            .enumerate()
            .filter(|(index, term)| direct.contains(&term.source) || unambiguous.contains(index))
            .map(|(_, term)| term)
            .collect())
    }

    pub fn alias_conflicts(&self) -> Result<Vec<AliasConflict>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT alias, first_source, second_source
                 FROM alias_conflicts ORDER BY alias, first_source, second_source",
            )
            .map_err(|error| format!("failed to prepare alias conflicts: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(AliasConflict {
                    alias: row.get(0)?,
                    first_source: row.get(1)?,
                    second_source: row.get(2)?,
                })
            })
            .map_err(|error| format!("failed to query alias conflicts: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("failed to read alias conflict: {error}")))
            .collect()
    }

    pub fn queue_extraction(&self, extraction: &PendingExtraction) -> Result<(), String> {
        let connection = self.connect()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO pending_term_extractions
                 (chapter_id, batch_key, source_text, target_text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    extraction.chapter_id,
                    extraction.batch_key,
                    extraction.source_text,
                    extraction.target_text,
                ],
            )
            .map_err(|error| format!("failed to queue term extraction: {error}"))?;
        Ok(())
    }

    pub fn pending_extractions(&self) -> Result<Vec<PendingExtraction>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT chapter_id, batch_key, source_text, target_text
                 FROM pending_term_extractions ORDER BY chapter_id, batch_key",
            )
            .map_err(|error| format!("failed to prepare pending extractions: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(PendingExtraction {
                    chapter_id: row.get(0)?,
                    batch_key: row.get(1)?,
                    source_text: row.get(2)?,
                    target_text: row.get(3)?,
                })
            })
            .map_err(|error| format!("failed to query pending extractions: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("failed to read pending extraction: {error}")))
            .collect()
    }

    pub fn complete_extraction(&self, chapter_id: &str, batch_key: &str) -> Result<(), String> {
        let connection = self.connect()?;
        connection
            .execute(
                "DELETE FROM pending_term_extractions
                 WHERE chapter_id = ?1 AND batch_key = ?2",
                params![chapter_id, batch_key],
            )
            .map_err(|error| format!("failed to complete term extraction: {error}"))?;
        Ok(())
    }
}

pub async fn extract_terms<C: TranslationClient + ?Sized>(
    client: &C,
    source_text: &str,
    target_text: &str,
    chapter: usize,
    max_retries: usize,
) -> Result<Vec<Term>, String> {
    let user = serde_json::json!({
        "chapter": chapter,
        "source": source_text,
        "target": target_text,
    })
    .to_string();
    let system = "TASK:TERM_EXTRACTION Extract names, places, organizations, domain terms, forms of address, speech habits, and fixed expressions whose translations should stay consistent. Return only JSON as {\"terms\":[...]}. Every term must contain source, target, reading, type, gender, aliases, first_chapter, note, and status=\"ok\". Return an empty array when nothing qualifies.";
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client.complete(system, &user).await {
            Ok(raw) => match serde_json::from_str::<ExtractionResponse>(&raw) {
                Ok(mut response) => {
                    let validation = response.terms.iter_mut().try_for_each(|term| {
                        term.first_chapter = chapter;
                        term.status = TermStatus::Ok;
                        validate_term(term)
                    });
                    match validation {
                        Ok(()) => return Ok(response.terms),
                        Err(error) => last_error = error,
                    }
                }
                Err(error) => last_error = format!("invalid term extraction JSON: {error}"),
            },
            Err(error) => last_error = error,
        }
        if attempt < max_retries {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
        }
    }
    Err(format!(
        "term extraction failed after {max_retries} retries: {last_error}"
    ))
}

fn initialize_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS terms (
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
             );",
        )
        .map_err(|error| format!("failed to initialize terms database: {error}"))
}

fn validate_term(term: &Term) -> Result<(), String> {
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

fn write_new_term(transaction: &Transaction<'_>, term: &Term) -> Result<(), String> {
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

fn merge_term_metadata(
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

fn record_candidate(
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

fn sync_aliases(
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
            transaction
                .execute(
                    "UPDATE terms SET status = 'conflict'
                     WHERE source IN (?1, ?2) AND status != 'resolved'",
                    params![source, other],
                )
                .map_err(|error| format!("failed to mark alias conflict: {error}"))?;
        }
    }
    Ok(())
}

fn find_term(transaction: &Transaction<'_>, source: &str) -> Result<Option<Term>, String> {
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

fn row_to_term(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredTerm> {
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

fn parse_stored_term(value: StoredTerm) -> Result<Term, String> {
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
    })
}

fn matches_text(haystack: &str, needle: &str) -> bool {
    let haystack = normalize(haystack);
    let needle = normalize(needle);
    if needle.is_empty() {
        return false;
    }
    if needle.chars().any(is_cjk) {
        return haystack.contains(&needle);
    }
    haystack.match_indices(&needle).any(|(start, value)| {
        let before = haystack[..start].chars().next_back();
        let end = start + value.len();
        let after = haystack[end..].chars().next();
        before.is_none_or(|value| !value.is_alphanumeric())
            && after.is_none_or(|value| !value.is_alphanumeric())
    })
}

fn normalize(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect()
}

fn is_cjk(value: char) -> bool {
    matches!(value, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

#[cfg(test)]
mod tests {
    use super::{extract_terms, PendingExtraction, Term, TermStatus, TermStore};
    use crate::llm::MockClient;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn store(name: &str) -> (TermStore, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-terms-{name}-{}-{nonce}.db",
            std::process::id()
        ));
        (TermStore::open(&path).expect("store should open"), path)
    }

    fn term(source: &str, target: &str) -> Term {
        Term {
            source: source.to_string(),
            target: target.to_string(),
            reading: None,
            term_type: "person".to_string(),
            gender: None,
            aliases: Vec::new(),
            first_chapter: 0,
            note: None,
            status: TermStatus::Ok,
        }
    }

    #[test]
    fn inserts_and_lists_terms() {
        let (store, path) = store("insert");
        assert_eq!(
            store
                .insert(&term("Alice", "爱丽丝"))
                .expect("term should insert"),
            TermStatus::Ok
        );
        let terms = store.list().expect("terms should list");
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].source, "Alice");
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[test]
    fn records_conflicts_without_overwriting_the_current_target() {
        let (store, path) = store("conflict");
        store
            .insert(&term("Alice", "爱丽丝"))
            .expect("first term should insert");
        assert_eq!(
            store
                .insert(&term("Alice", "艾丽斯"))
                .expect("conflict should record"),
            TermStatus::Conflict
        );
        let terms = store.list().expect("terms should list");
        assert_eq!(terms[0].target, "爱丽丝");
        assert_eq!(terms[0].status, TermStatus::Conflict);
        assert_eq!(store.conflicts().expect("conflicts should list").len(), 2);
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[test]
    fn resolves_to_an_arbitrary_target_and_hides_settled_conflicts() {
        let (store, path) = store("resolve");
        store
            .insert(&term("Alice", "爱丽丝"))
            .expect("first term should insert");
        store
            .insert(&term("Alice", "艾丽斯"))
            .expect("conflict should record");
        store
            .resolve("Alice", "阿丽丝")
            .expect("term should resolve");
        let terms = store.list().expect("terms should list");
        assert_eq!(terms[0].target, "阿丽丝");
        assert_eq!(terms[0].status, TermStatus::Resolved);
        assert!(store.conflicts().expect("conflicts should list").is_empty());
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[test]
    fn filters_relevant_terms_by_source_and_alias() {
        let (store, path) = store("relevant");
        let mut alice = term("Alice", "爱丽丝");
        alice.aliases.push("ＡＬＩＣＩＡ".to_string());
        store.insert(&alice).expect("Alice should insert");
        store
            .insert(&term("王都", "王都"))
            .expect("CJK term should insert");
        store.insert(&term("cat", "猫")).expect("cat should insert");

        let relevant = store
            .relevant("Alicia entered the 王都. Concatenate stayed behind.")
            .expect("relevant terms should filter");
        assert_eq!(relevant.len(), 2);
        assert!(relevant.iter().any(|term| term.source == "Alice"));
        assert!(relevant.iter().any(|term| term.source == "王都"));
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[test]
    fn records_ambiguous_aliases_and_does_not_inject_them() {
        let (store, path) = store("alias-conflict");
        let mut alice = term("Alice", "爱丽丝");
        alice.aliases.push("Captain".to_string());
        let mut bob = term("Bob", "鲍勃");
        bob.aliases.push("captain".to_string());
        store.insert(&alice).expect("Alice should insert");
        assert_eq!(
            store.insert(&bob).expect("Bob should insert"),
            TermStatus::Conflict
        );

        assert_eq!(
            store.alias_conflicts().expect("aliases should list").len(),
            1
        );
        assert!(store
            .relevant("The CAPTAIN entered.")
            .expect("relevant terms should filter")
            .is_empty());
        assert!(store
            .list()
            .expect("terms should list")
            .iter()
            .all(|term| term.status == TermStatus::Conflict));
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[test]
    fn persists_pending_extractions_until_completed() {
        let (store, path) = store("pending");
        let extraction = PendingExtraction {
            chapter_id: "chapter-1".to_string(),
            batch_key: "batch-1".to_string(),
            source_text: "Alice".to_string(),
            target_text: "爱丽丝".to_string(),
        };
        store
            .queue_extraction(&extraction)
            .expect("extraction should queue");
        assert_eq!(
            store
                .pending_extractions()
                .expect("pending extraction should list"),
            vec![extraction]
        );
        store
            .complete_extraction("chapter-1", "batch-1")
            .expect("extraction should complete");
        assert!(store
            .pending_extractions()
            .expect("pending extraction should list")
            .is_empty());
        std::fs::remove_file(path).expect("database should be removed");
    }

    #[tokio::test]
    async fn extracts_stable_terms_with_mock_client() {
        let terms = extract_terms(&MockClient, "Alice arrived.", "爱丽丝到了。", 2, 0)
            .await
            .expect("mock extraction should work");
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].source, "Alice");
        assert_eq!(terms[0].first_chapter, 2);
    }
}
