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

/// A repeatable, auditable candidate found in the complete source text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FullTextCandidate {
    pub source: String,
    pub normalized: String,
    pub occurrences: usize,
    pub first_chapter: usize,
    pub last_chapter: usize,
    pub contexts: Vec<String>,
    pub sources: Vec<String>,
    pub surface_forms: Vec<String>,
    pub category: String,
    pub variants: Vec<String>,
    pub proposed_target: String,
    pub confidence: Option<f64>,
    pub reason: String,
    pub status: ReviewStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Candidate,
    Confirmed,
    Rejected,
    Variant,
}

/// Finds repeated 1-5 gram phrases without deciding whether they are terms.
pub fn scan_full_text(chapters: &[(usize, &str)]) -> Vec<FullTextCandidate> {
    let mut found: HashMap<String, FullTextCandidate> = HashMap::new();
    for &(chapter, text) in chapters {
        let tokens = scan_tokens(text);
        for start in 0..tokens.len() {
            for width in 1..=5 {
                let end = start + width;
                if end > tokens.len() {
                    break;
                }
                if tokens[start..end].windows(2).any(|pair| {
                    text[pair[0].1..pair[1].0]
                        .chars()
                        .any(|c| !c.is_whitespace() || c == '\n' || c == '\r')
                }) {
                    break;
                }
                let source = text[tokens[start].0..tokens[end - 1].1].to_string();
                let normalized = normalize(&source);
                if normalized.trim().is_empty() {
                    continue;
                }
                let entry = found
                    .entry(normalized.clone())
                    .or_insert_with(|| FullTextCandidate {
                        source: source.clone(),
                        normalized: normalized.clone(),
                        occurrences: 0,
                        first_chapter: chapter,
                        last_chapter: chapter,
                        contexts: Vec::new(),
                        sources: vec!["frequency".to_string()],
                        surface_forms: Vec::new(),
                        category: "term".to_string(),
                        variants: Vec::new(),
                        proposed_target: String::new(),
                        confidence: None,
                        reason: "Repeated source phrase".to_string(),
                        status: ReviewStatus::Candidate,
                    });
                entry.occurrences += 1;
                entry.first_chapter = entry.first_chapter.min(chapter);
                entry.last_chapter = entry.last_chapter.max(chapter);
                if !entry.surface_forms.contains(&source) {
                    entry.surface_forms.push(source);
                }
                if entry.contexts.len() < 3 {
                    let before: String = text[..tokens[start].0]
                        .chars()
                        .rev()
                        .take(60)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    entry.contexts.push(format!(
                        "{before}{}",
                        text[tokens[start].0..]
                            .chars()
                            .take(120)
                            .collect::<String>()
                    ));
                }
            }
        }
    }
    let mut values: Vec<_> = found
        .into_values()
        .filter(|candidate| candidate.occurrences > 1)
        .collect();
    values.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.normalized.cmp(&b.normalized))
    });
    values
}

fn scan_tokens(text: &str) -> Vec<(usize, usize)> {
    let mut tokens = Vec::new();
    let mut word = None;
    for (offset, c) in text.char_indices() {
        let separate = is_cjk(c);
        if separate || (!c.is_alphanumeric() && !unicode_normalization::char::is_combining_mark(c))
        {
            if let Some(start) = word.take() {
                tokens.push((start, offset));
            }
            if separate {
                tokens.push((offset, offset + c.len_utf8()));
            }
        } else {
            word.get_or_insert(offset);
        }
    }
    if let Some(start) = word {
        tokens.push((start, text.len()));
    }
    tokens
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
    terms: Vec<ExtractionRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionRecord {
    source: String,
    category: String,
    variants: Vec<String>,
    evidence_count: usize,
    contexts: Vec<String>,
    proposed_target: String,
    confidence: f64,
    reason: String,
}

#[derive(Debug, Clone)]
pub struct TermStore {
    path: std::path::PathBuf,
}

impl TermStore {
    pub fn candidates(&self) -> Result<Vec<FullTextCandidate>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare("SELECT record FROM discovered_terms ORDER BY normalized, target")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }

    pub fn discover(&self, candidate: &FullTextCandidate) -> Result<(), String> {
        let connection = self.connect()?;
        Self::discover_on(&connection, candidate)
    }

    fn discover_on(connection: &Connection, candidate: &FullTextCandidate) -> Result<(), String> {
        let mut value = candidate.clone();
        if value.source.trim().is_empty()
            || value.normalized != normalize(&value.source)
            || value.sources.is_empty()
            || value
                .sources
                .iter()
                .any(|s| !["llm", "frequency", "ner", "translation-drift"].contains(&s.as_str()))
        {
            return Err("invalid candidate source or provenance".to_string());
        }
        let previous: Option<String> = connection
            .query_row(
                "SELECT record FROM discovered_terms WHERE normalized = ?1 AND target = ?2 AND drift = ?3",
                params![value.normalized, value.proposed_target, value.sources.iter().any(|s| s == "translation-drift")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(previous) = previous {
            let old: FullTextCandidate =
                serde_json::from_str(&previous).map_err(|e| e.to_string())?;
            value.status = old.status;
            value.occurrences = value.occurrences.max(old.occurrences);
            value.first_chapter = value.first_chapter.min(old.first_chapter);
            value.last_chapter = value.last_chapter.max(old.last_chapter);
            for source in old.sources {
                if !value.sources.contains(&source) {
                    value.sources.push(source);
                }
            }
            for surface in old.surface_forms {
                if !value.surface_forms.contains(&surface) {
                    value.surface_forms.push(surface);
                }
            }
            for context in old.contexts {
                if value.contexts.len() < 3 && !value.contexts.contains(&context) {
                    value.contexts.push(context);
                }
            }
            for variant in old.variants {
                if !value.variants.contains(&variant) {
                    value.variants.push(variant);
                }
            }
        }
        // LLM proposals inherit the complete-book evidence of the frequency scan.
        if !value.proposed_target.is_empty()
            && !value.sources.iter().any(|s| s == "translation-drift")
        {
            let baseline: Option<String> = connection.query_row(
                "SELECT record FROM discovered_terms WHERE normalized = ?1 AND target = '' AND drift = 0",
                [&value.normalized], |row| row.get(0),
            ).optional().map_err(|e| e.to_string())?;
            if let Some(baseline) = baseline {
                let baseline: FullTextCandidate =
                    serde_json::from_str(&baseline).map_err(|e| e.to_string())?;
                value.occurrences = value.occurrences.max(baseline.occurrences);
                value.first_chapter = value.first_chapter.min(baseline.first_chapter);
                value.last_chapter = value.last_chapter.max(baseline.last_chapter);
                for source in baseline.sources {
                    if !value.sources.contains(&source) {
                        value.sources.push(source);
                    }
                }
                for surface in baseline.surface_forms {
                    if !value.surface_forms.contains(&surface) {
                        value.surface_forms.push(surface);
                    }
                }
            }
        }
        connection.execute("INSERT OR REPLACE INTO discovered_terms (normalized, target, record, drift) VALUES (?1, ?2, ?3, ?4)",
            params![value.normalized, value.proposed_target, serde_json::to_string(&value).map_err(|e| e.to_string())?, value.sources.iter().any(|s| s == "translation-drift")]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn review_candidate(
        &self,
        normalized: &str,
        proposed_target: &str,
        status: ReviewStatus,
        target: &str,
    ) -> Result<(), String> {
        self.review_candidate_with_origin(normalized, proposed_target, status, target, false)
    }

    pub fn review_candidate_with_origin(
        &self,
        normalized: &str,
        proposed_target: &str,
        status: ReviewStatus,
        target: &str,
        drift: bool,
    ) -> Result<(), String> {
        let mut candidate = self
            .candidates()?
            .into_iter()
            .find(|c| {
                c.normalized == normalized
                    && c.proposed_target == proposed_target
                    && c.sources.iter().any(|s| s == "translation-drift") == drift
            })
            .ok_or_else(|| "candidate not found".to_string())?;
        if candidate.status != ReviewStatus::Candidate {
            return Err("candidate has already been reviewed".to_string());
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        if status == ReviewStatus::Confirmed {
            let canonical_source = self
                .list()?
                .into_iter()
                .find(|term| normalize(&term.source) == normalized)
                .map(|term| term.source);
            let mut term = Term {
                source: canonical_source.unwrap_or_else(|| candidate.source.clone()),
                target: target.trim().to_string(),
                reading: None,
                term_type: candidate.category.clone(),
                gender: None,
                aliases: candidate.variants.clone(),
                first_chapter: candidate.first_chapter,
                note: Some(candidate.reason.clone()),
                status: TermStatus::Resolved,
            };
            for surface in &candidate.surface_forms {
                if surface != &term.source && !term.aliases.contains(surface) {
                    term.aliases.push(surface.clone());
                }
            }
            validate_term(&term)?;
            if let Some(existing) = find_term(&transaction, &term.source)? {
                merge_term_metadata(&transaction, &existing, &term)?;
                for alias in existing.aliases {
                    if !term.aliases.contains(&alias) {
                        term.aliases.push(alias);
                    }
                }
                transaction
                    .execute(
                        "UPDATE terms SET target = ?2, status = 'resolved' WHERE source = ?1",
                        params![term.source, term.target],
                    )
                    .map_err(|e| e.to_string())?;
            } else {
                write_new_term(&transaction, &term)?;
            }
            sync_aliases(&transaction, &term.source, &term.aliases)?;
        } else if status == ReviewStatus::Variant {
            let mut canonical = self
                .list()?
                .into_iter()
                .find(|term| term.source == target && term.status == TermStatus::Resolved)
                .ok_or_else(|| "variant must name an existing confirmed source term".to_string())?;
            if normalize(&canonical.source) == candidate.normalized {
                return Err("a term cannot be its own variant".to_string());
            }
            canonical.aliases.push(candidate.source.clone());
            merge_term_metadata(&transaction, &canonical, &canonical)?;
            sync_aliases(&transaction, &canonical.source, &canonical.aliases)?;
        }
        candidate.status = status;
        transaction
            .execute(
                "UPDATE discovered_terms SET record = ?3 WHERE normalized = ?1 AND target = ?2 AND drift = ?4",
                params![
                    normalized,
                    proposed_target,
                    serde_json::to_string(&candidate).map_err(|e| e.to_string())?, drift
                ],
            )
            .map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn scan(&self, chapters: &[crate::model::Chapter]) -> Result<(), String> {
        let texts: Vec<String> = chapters
            .iter()
            .map(|chapter| {
                chapter
                    .segments
                    .iter()
                    .map(|s| s.source.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        let inputs: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (i, text.as_str()))
            .collect();
        let candidates = scan_full_text(&inputs);
        let mut connection = self.connect()?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        for candidate in candidates {
            Self::discover_on(&transaction, &candidate)?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_analysis_term(&self, term: &Term) -> Result<(), String> {
        self.discover(&FullTextCandidate {
            source: term.source.clone(),
            normalized: normalize(&term.source),
            occurrences: 0,
            first_chapter: term.first_chapter,
            last_chapter: term.first_chapter,
            contexts: Vec::new(),
            sources: vec!["llm".to_string()],
            surface_forms: vec![term.source.clone()],
            category: term.term_type.clone(),
            variants: term.aliases.clone(),
            proposed_target: term.target.clone(),
            confidence: None,
            reason: term.note.clone().unwrap_or_default(),
            status: ReviewStatus::Candidate,
        })
    }

    pub fn record_drift(&self, source: &str, target: &str, chapter: usize) -> Result<(), String> {
        for term in self.relevant(source)? {
            if !matches_text(target, &term.target) {
                self.discover(&FullTextCandidate { source: term.source.clone(), normalized: normalize(&term.source), occurrences: 1,
                    first_chapter: chapter, last_chapter: chapter, contexts: vec![format!("source: {source}\ntarget: {target}")],
                    sources: vec!["translation-drift".to_string()], surface_forms: vec![term.source], category: term.term_type,
                    variants: term.aliases, proposed_target: String::new(), confidence: None,
                    reason: format!("Confirmed target '{}' missing; review alignment before proposing a replacement", term.target), status: ReviewStatus::Candidate })?;
            }
        }
        Ok(())
    }

    pub fn record_extracted(&self, mut candidate: FullTextCandidate) -> Result<(), String> {
        if !candidate.proposed_target.is_empty()
            && self.list()?.iter().any(|term| {
                term.status == TermStatus::Resolved
                    && (normalize(&term.source) == candidate.normalized
                        || term
                            .aliases
                            .iter()
                            .any(|alias| normalize(alias) == candidate.normalized))
                    && normalize(&term.target) != normalize(&candidate.proposed_target)
            })
        {
            candidate.sources.push("translation-drift".to_string());
        }
        self.discover(&candidate)
    }
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
                merge_term_metadata(&transaction, &existing, term)?;
                let mut aliases = existing.aliases.clone();
                for alias in &term.aliases {
                    if !aliases.contains(alias) {
                        aliases.push(alias.clone());
                    }
                }
                sync_aliases(&transaction, &term.source, &aliases)?;
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
        let terms: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|term| term.status == TermStatus::Resolved)
            .collect();
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
    Ok(
        extract_candidates(client, source_text, target_text, chapter, max_retries)
            .await?
            .into_iter()
            .map(|candidate| Term {
                source: candidate.source,
                target: candidate.proposed_target,
                reading: None,
                term_type: candidate.category,
                gender: None,
                aliases: candidate.variants,
                first_chapter: chapter,
                note: Some(candidate.reason),
                status: TermStatus::Ok,
            })
            .collect(),
    )
}

pub async fn extract_candidates<C: TranslationClient + ?Sized>(
    client: &C,
    source_text: &str,
    target_text: &str,
    chapter: usize,
    max_retries: usize,
) -> Result<Vec<FullTextCandidate>, String> {
    extract_candidates_with_context(
        client,
        source_text,
        target_text,
        chapter,
        max_retries,
        &ExtractionContext {
            source_language: "auto",
            target_language: "zh-CN",
            confirmed_terms: &[],
        },
    )
    .await
}

pub struct ExtractionContext<'a> {
    pub source_language: &'a str,
    pub target_language: &'a str,
    pub confirmed_terms: &'a [Term],
}

pub async fn extract_candidates_with_context<C: TranslationClient + ?Sized>(
    client: &C,
    source_text: &str,
    target_text: &str,
    chapter: usize,
    max_retries: usize,
    context: &ExtractionContext<'_>,
) -> Result<Vec<FullTextCandidate>, String> {
    let user = serde_json::json!({
        "chapter": chapter,
        "source": source_text,
        "target": target_text,
        "source_language": context.source_language,
        "target_language": context.target_language,
        "confirmed_terms": context.confirmed_terms.iter().filter(|term| term.status == TermStatus::Resolved).collect::<Vec<_>>(),
    })
    .to_string();
    let system = "TASK:TERM_EXTRACTION Extract names, places, organizations, domain terms, forms of address, speech habits, and fixed expressions. Return only JSON as {\"terms\":[...]}. Each record must contain exactly: source (string), category (person/place/organization/term/appellation/speech/fixed_expr), variants (string array), evidence_count (positive integer), contexts (nonempty array of exact source excerpts containing the term), proposed_target (string in target_language; empty if uncertain), confidence (number 0..1), reason (nonempty string). Prefer a low-confidence candidate over omission. Candidates may be decided using context. Never invent source evidence. CONFIRMED TERMINOLOGY CONSTRAINTS: confirmed_terms is authoritative; never propose rewriting confirmed targets. When target text is supplied, report actual observed translations, including any drift, without correcting them or treating them as authoritative. Return an empty terms array if none are found.";
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client.complete(system, &user).await {
            Ok(output) => match crate::llm::parse_json_response::<ExtractionResponse>(&output.text)
            {
                Ok(response) => {
                    let validation = response.terms.iter().try_for_each(|record| {
                        let term = Term {
                            source: record.source.clone(),
                            target: "validation".to_string(),
                            reading: None,
                            term_type: record.category.clone(),
                            gender: None,
                            aliases: record.variants.clone(),
                            first_chapter: chapter,
                            note: None,
                            status: TermStatus::Ok,
                        };
                        validate_term(&term)?;
                        if !record.confidence.is_finite()
                            || !(0.0..=1.0).contains(&record.confidence)
                            || record.evidence_count == 0
                            || record.evidence_count > match_count(source_text, &record.source)
                            || record.contexts.is_empty()
                            || record.reason.trim().is_empty()
                            || !matches_text(source_text, &record.source)
                            || record
                                .variants
                                .iter()
                                .any(|variant| !matches_text(source_text, variant))
                            || record.contexts.iter().any(|c| {
                                c.trim().is_empty()
                                    || !source_text.contains(c)
                                    || !matches_text(c, &record.source)
                            })
                            || (!target_text.is_empty()
                                && !record.proposed_target.is_empty()
                                && !target_text.contains(&record.proposed_target))
                        {
                            return Err(
                                "invalid candidate evidence, target, or confidence".to_string()
                            );
                        }
                        Ok(())
                    });
                    match validation {
                        Ok(()) => {
                            return Ok(response
                                .terms
                                .into_iter()
                                .map(|record| FullTextCandidate {
                                    normalized: normalize(&record.source),
                                    surface_forms: vec![record.source.clone()],
                                    occurrences: match_count(source_text, &record.source),
                                    source: record.source,
                                    first_chapter: chapter,
                                    last_chapter: chapter,
                                    contexts: record.contexts,
                                    sources: vec!["llm".to_string()],
                                    category: record.category,
                                    variants: record.variants,
                                    proposed_target: record.proposed_target,
                                    confidence: Some(record.confidence),
                                    reason: record.reason,
                                    status: ReviewStatus::Candidate,
                                })
                                .collect())
                        }
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
             CREATE TABLE IF NOT EXISTS discovered_terms (
                 normalized TEXT NOT NULL,
                 target TEXT NOT NULL,
                 record TEXT NOT NULL,
                 drift INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(normalized, target, drift)
             );
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
    match_count(haystack, needle) > 0
}

fn match_count(haystack: &str, needle: &str) -> usize {
    let haystack = normalize(haystack);
    let needle = normalize(needle);
    if needle.is_empty() {
        return 0;
    }
    if needle.chars().any(is_cjk) {
        return haystack
            .char_indices()
            .filter(|(start, _)| haystack[*start..].starts_with(&needle))
            .count();
    }
    haystack
        .match_indices(&needle)
        .filter(|(start, value)| {
            let before = haystack[..*start].chars().next_back();
            let end = start + value.len();
            let after = haystack[end..].chars().next();
            before.is_none_or(|value| !value.is_alphanumeric())
                && after.is_none_or(|value| !value.is_alphanumeric())
        })
        .count()
}

fn normalize(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect()
}

fn is_cjk(value: char) -> bool {
    matches!(value, '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ff66}'..='\u{ff9f}' | '\u{ac00}'..='\u{d7af}' | '\u{20000}'..='\u{2fa1f}')
}

#[cfg(test)]
mod tests {
    use super::{
        extract_terms, scan_full_text, PendingExtraction, ReviewStatus, Term, TermStatus, TermStore,
    };
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
    fn scans_cjk_and_normalized_phrases_with_local_evidence() {
        assert!(super::matches_text("アリスが来た。", "ｱﾘｽ"));
        let text = format!("{}知我麻社に来た。", "前文。".repeat(100));
        let found = scan_full_text(&[
            (0, &text),
            (3, "また知我麻社へ。"),
            (4, "Ｃａｆｅ\u{301} Club. café club!"),
        ]);
        let society = found
            .iter()
            .find(|c| c.source == "知我麻社")
            .expect("CJK name");
        assert_eq!(society.occurrences, 2);
        assert_eq!((society.first_chapter, society.last_chapter), (0, 3));
        assert!(society.contexts.iter().all(|c| c.contains("知我麻社")));
        let cafe = found
            .iter()
            .find(|c| c.normalized == "café club")
            .expect("normalized phrase");
        assert_eq!(cafe.occurrences, 2);
        assert_eq!(cafe.surface_forms.len(), 2);
        assert!(!found.iter().any(|c| c.normalized.contains("club café")));
        assert!(scan_full_text(&[(0, "one two"), (1, "two three")])
            .iter()
            .all(|c| c.source != "one two two three"));
    }

    #[test]
    fn candidate_review_controls_injection_and_preserves_drift() {
        let (store, path) = store("review");
        let candidate = scan_full_text(&[(0, "Alice Alice")])
            .into_iter()
            .find(|c| c.source == "Alice")
            .unwrap();
        store.discover(&candidate).unwrap();
        assert!(store.relevant("Alice").unwrap().is_empty());
        assert!(store
            .review_candidate("alice", "", ReviewStatus::Confirmed, " ")
            .is_err());
        assert!(store.list().unwrap().is_empty());
        store
            .review_candidate("alice", "", ReviewStatus::Confirmed, "爱丽丝")
            .unwrap();
        assert_eq!(store.relevant("ALICE").unwrap()[0].target, "爱丽丝");
        store.discover(&candidate).unwrap();
        assert_eq!(
            store.candidates().unwrap()[0].status,
            ReviewStatus::Confirmed
        );
        assert_eq!(store.candidates().unwrap()[0].occurrences, 2);
        let mut drift = candidate.clone();
        drift.proposed_target = "艾丽斯".to_string();
        drift.sources = vec!["translation-drift".to_string()];
        store.discover(&drift).unwrap();
        assert_eq!(store.relevant("Alice").unwrap()[0].target, "爱丽丝");
        store
            .review_candidate_with_origin("alice", "艾丽斯", ReviewStatus::Rejected, "", true)
            .unwrap();
        store.discover(&drift).unwrap();
        assert!(store
            .candidates()
            .unwrap()
            .iter()
            .any(|c| c.proposed_target == "艾丽斯" && c.status == ReviewStatus::Rejected));
        store.record_drift("Alice", "艾丽斯", 3).unwrap();
        assert!(store
            .candidates()
            .unwrap()
            .iter()
            .any(|c| c.proposed_target.is_empty()
                && c.sources.iter().any(|s| s == "translation-drift")
                && c.status == ReviewStatus::Candidate));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reviewed_variants_match_the_confirmed_term() {
        let (store, path) = store("variant-review");
        let mut alice = term("Alice", "爱丽丝");
        alice.status = TermStatus::Resolved;
        store.insert(&alice).unwrap();
        let candidate = scan_full_text(&[(0, "Alicia Alicia")]).remove(0);
        store.discover(&candidate).unwrap();
        assert!(store
            .review_candidate(&candidate.normalized, "", ReviewStatus::Variant, "missing")
            .is_err());
        store
            .review_candidate(&candidate.normalized, "", ReviewStatus::Variant, "Alice")
            .unwrap();
        assert_eq!(store.relevant("ALICIA").unwrap()[0].source, "Alice");
        assert_eq!(store.candidates().unwrap()[0].status, ReviewStatus::Variant);
        store.record_drift("Alicia", "艾丽斯", 4).unwrap();
        assert!(store
            .candidates()
            .unwrap()
            .iter()
            .any(|c| c.sources.contains(&"translation-drift".to_string()) && c.first_chapter == 4));
        assert_eq!(store.list().unwrap()[0].target, "爱丽丝");
        std::fs::remove_file(path).unwrap();
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
        store.resolve("Alice", "爱丽丝").unwrap();
        store
            .insert(&term("王都", "王都"))
            .expect("CJK term should insert");
        store.insert(&term("cat", "猫")).expect("cat should insert");
        store.resolve("王都", "王都").unwrap();
        store.resolve("cat", "猫").unwrap();

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

    struct FixedClient(serde_json::Value);

    #[async_trait::async_trait]
    impl crate::llm::TranslationClient for FixedClient {
        async fn complete(
            &self,
            system: &str,
            _user: &str,
        ) -> Result<crate::llm::CompletionOutput, String> {
            assert!(system.contains("low-confidence candidate over omission"));
            Ok(crate::llm::CompletionOutput {
                text: self.0.to_string(),
                usage: Default::default(),
            })
        }
    }

    #[tokio::test]
    async fn extraction_keeps_uncertainty_and_rejects_invalid_evidence() {
        let record = serde_json::json!({"source":"Alice", "category":"person", "variants":[], "evidence_count":1,
            "contexts":["Alice arrived."], "proposed_target":"", "confidence":0.1, "reason":"Uncertain translation"});
        let response = serde_json::json!({"terms":[record.clone()]});
        let candidates = super::extract_candidates(
            &FixedClient(response),
            "Alice arrived.",
            "爱丽丝到了。",
            0,
            0,
        )
        .await
        .unwrap();
        assert_eq!(candidates[0].confidence, Some(0.1));
        assert!(candidates[0].proposed_target.is_empty());
        for (field, value) in [
            ("confidence", serde_json::json!(1.2)),
            ("contexts", serde_json::json!(["Invented context"])),
            ("evidence_count", serde_json::json!(20)),
            ("proposed_target", serde_json::json!("未出现")),
            ("extra", serde_json::json!(true)),
        ] {
            let mut invalid = record.clone();
            invalid[field] = value;
            assert!(
                super::extract_candidates(
                    &FixedClient(serde_json::json!({"terms":[invalid]})),
                    "Alice arrived.",
                    "爱丽丝到了。",
                    0,
                    0
                )
                .await
                .is_err(),
                "accepted {field}"
            );
        }
        let mut missing = record;
        missing.as_object_mut().unwrap().remove("confidence");
        assert!(super::extract_candidates(
            &FixedClient(serde_json::json!({"terms":[missing]})),
            "Alice arrived.",
            "",
            0,
            0
        )
        .await
        .is_err());
    }
}
