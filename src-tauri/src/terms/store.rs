//! 术语库操作入口，封装 SQLite 事务。

use super::matching::relevant_terms;
use super::sqlite::{
    audit, find_term, initialize_schema, merge_term_metadata, parse_stored_term, read_policy,
    read_rule, record_candidate, record_conflict, record_evidence, row_to_term, sync_aliases,
    validate_term, write_new_term, write_rule,
};
use super::{
    AliasConflict, ConflictCandidate, PendingExtraction, Term, TermCandidate, TermConflict,
    TermEvidence, TermPolicy, TermStatus,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

/// 项目术语库，持有 `terms.db` 路径，每次操作独立打开连接。
#[derive(Debug, Clone)]
pub struct TermStore {
    path: std::path::PathBuf,
}

impl TermStore {
    /// 打开（必要时创建）术语库并初始化表结构。
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

    /// 写入术语。
    ///
    /// 返回最终状态：新术语为 `Ok`；与现有译名冲突时为 `Conflict`
    /// （现有译名不会被覆盖）；人工已固定译名时为 `Resolved`。
    pub fn insert(&self, term: &Term) -> Result<TermStatus, String> {
        self.insert_with_evidence(term, "", "")
    }

    /// 写入术语并记录原文/译文证据片段。
    pub fn insert_with_evidence(
        &self,
        term: &Term,
        source_text: &str,
        target_text: &str,
    ) -> Result<TermStatus, String> {
        validate_term(term)?;
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("failed to start term transaction: {error}"))?;
        let existing = find_term(&transaction, &term.source)?;
        let policy = read_policy(&transaction, &term.source)?;
        if policy == TermPolicy::Ignored {
            transaction
                .commit()
                .map_err(|error| format!("failed to commit term transaction: {error}"))?;
            return Ok(TermStatus::Resolved);
        }
        let status = match existing {
            None => {
                write_new_term(&transaction, term)?;
                sync_aliases(&transaction, &term.source, &term.aliases)?;
                record_evidence(&transaction, term, source_text, target_text)?;
                TermStatus::Ok
            }
            Some(existing) if existing.target == term.target => {
                merge_term_metadata(&transaction, &existing, term)?;
                record_evidence(&transaction, term, source_text, target_text)?;
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
                record_evidence(&transaction, term, source_text, target_text)?;
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
                record_conflict(&transaction, &term.source, &term.target)?;
                if matches!(policy, TermPolicy::Fixed | TermPolicy::NonFixed) {
                    transaction
                        .execute(
                            "UPDATE term_conflicts SET resolved = 1, resolved_at = CURRENT_TIMESTAMP
                             WHERE source = ?1 AND target = ?2",
                            params![term.source, term.target],
                        )
                        .map_err(|error| format!("failed to settle term conflict: {error}"))?;
                    TermStatus::Resolved
                } else {
                    transaction
                        .execute(
                            "UPDATE terms SET status = 'conflict' WHERE source = ?1",
                            [&term.source],
                        )
                        .map_err(|error| format!("failed to mark term conflict: {error}"))?;
                    TermStatus::Conflict
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

    /// 列出全部术语，并附带人工策略与固定译名。
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
        let mut terms = rows
            .map(|row| row.map_err(|error| format!("failed to read term: {error}")))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(parse_stored_term)
            .collect::<Result<Vec<_>, _>>()?;
        for term in &mut terms {
            let (policy, target) = read_rule(&connection, &term.source)?;
            term.policy = policy;
            term.manual_target = target;
        }
        Ok(terms)
    }

    /// 列出仍存在冲突的术语的候选译名。
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

    /// 人工裁定：把术语译名固定为 `target` 并关闭相关冲突。
    pub fn resolve(&self, source: &str, target: &str) -> Result<(), String> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err("term source and resolved target must not be empty".to_string());
        }
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let action = if read_policy(&transaction, source)? == TermPolicy::Fixed {
            "modify_resolution"
        } else {
            "resolve"
        };
        let changed = transaction
            .execute(
                "UPDATE terms SET target = ?2, status = 'resolved' WHERE source = ?1",
                params![source, target],
            )
            .map_err(|error| format!("failed to resolve term: {error}"))?;
        if changed == 0 {
            return Err(format!("term not found: {source}"));
        }
        write_rule(&transaction, source, TermPolicy::Fixed, Some(target))?;
        transaction.execute(
            "UPDATE term_conflicts SET resolved = 1, resolved_at = CURRENT_TIMESTAMP WHERE source = ?1 AND resolved = 0",
            [source],
        ).map_err(|error| format!("failed to settle term conflicts: {error}"))?;
        audit(
            &transaction,
            source,
            action,
            serde_json::json!({"target": target}),
        )?;
        transaction
            .commit()
            .map_err(|error| format!("failed to commit term resolution: {error}"))
    }

    /// 撤销人工裁定，恢复自动策略。
    pub fn undo_resolution(&self, source: &str) -> Result<(), String> {
        self.set_policy(source, TermPolicy::Automatic)
    }

    /// 设置术语策略。
    ///
    /// 恢复为 `Automatic` 时会重新打开历史证据中的冲突；设置为其他策略
    /// 会把现有冲突标记为已解决。
    pub fn set_policy(&self, source: &str, policy: TermPolicy) -> Result<(), String> {
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM terms WHERE source = ?1)",
                [source],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("failed to query term: {error}"))?;
        if !exists {
            return Err(format!("term not found: {source}"));
        }
        let target = if policy == TermPolicy::Fixed {
            transaction
                .query_row(
                    "SELECT target FROM terms WHERE source = ?1",
                    [source],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?
        } else {
            None
        };
        write_rule(&transaction, source, policy, target.as_deref())?;
        let unresolved = if policy == TermPolicy::Automatic {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO term_conflicts (source, target)
                     SELECT e.source, e.target FROM term_evidence e
                     JOIN terms t ON t.source = e.source
                     WHERE e.source = ?1 AND e.target != t.target",
                    [source],
                )
                .map_err(|error| format!("failed to restore term conflicts: {error}"))?;
            transaction
                .execute(
                    "UPDATE term_conflicts SET resolved = 0, resolved_at = NULL WHERE source = ?1",
                    [source],
                )
                .map_err(|error| format!("failed to reopen term conflicts: {error}"))?;
            transaction
                .query_row(
                    "SELECT COUNT(*) FROM term_conflicts WHERE source = ?1 AND resolved = 0",
                    [source],
                    |row| row.get::<_, usize>(0),
                )
                .map_err(|error| error.to_string())?
        } else {
            transaction.execute(
                "UPDATE term_conflicts SET resolved = 1, resolved_at = CURRENT_TIMESTAMP WHERE source = ?1 AND resolved = 0",
                [source],
            ).map_err(|error| format!("failed to settle term conflicts: {error}"))?;
            0
        };
        transaction
            .execute(
                "UPDATE terms SET status = ?2 WHERE source = ?1",
                params![
                    source,
                    if unresolved > 0 {
                        "conflict"
                    } else if policy == TermPolicy::Automatic {
                        "ok"
                    } else {
                        "resolved"
                    }
                ],
            )
            .map_err(|error| format!("failed to update term status: {error}"))?;
        audit(&transaction, source, policy.as_str(), serde_json::json!({}))?;
        transaction
            .commit()
            .map_err(|error| format!("failed to commit term policy: {error}"))
    }

    /// 汇总冲突详情；`include_resolved` 为 true 时包含已裁定的事件。
    pub fn conflict_details(&self, include_resolved: bool) -> Result<Vec<TermConflict>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT source, target, resolved FROM term_conflicts
             WHERE ?1 OR resolved = 0 ORDER BY source, id",
            )
            .map_err(|error| format!("failed to prepare conflict details: {error}"))?;
        let events = statement
            .query_map([include_resolved], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })
            .map_err(|error| format!("failed to query conflict details: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(statement);
        let mut sources = Vec::<String>::new();
        for (source, _, _) in &events {
            if !sources.contains(source) {
                sources.push(source.clone());
            }
        }
        let terms = self
            .list()?
            .into_iter()
            .map(|term| (term.source.clone(), term))
            .collect::<HashMap<_, _>>();
        let mut result = Vec::new();
        for source in sources {
            let Some(term) = terms.get(&source) else {
                continue;
            };
            let mut evidence_statement = connection
                .prepare(
                    "SELECT target, chapter, source_excerpt, target_excerpt FROM term_evidence
                 WHERE source = ?1 ORDER BY id",
                )
                .map_err(|error| error.to_string())?;
            let rows = evidence_statement
                .query_map([&source], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, usize>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let mut candidates = Vec::<ConflictCandidate>::new();
            for (target, chapter, source_excerpt, target_excerpt) in rows {
                let index = candidates
                    .iter()
                    .position(|item| item.target == target)
                    .unwrap_or_else(|| {
                        candidates.push(ConflictCandidate {
                            target: target.clone(),
                            occurrences: 0,
                            chapters: Vec::new(),
                            evidence: Vec::new(),
                        });
                        candidates.len() - 1
                    });
                let candidate = &mut candidates[index];
                candidate.occurrences += 1;
                if !candidate.chapters.contains(&chapter) {
                    candidate.chapters.push(chapter);
                }
                candidate.evidence.push(TermEvidence {
                    chapter,
                    source_excerpt,
                    target_excerpt,
                });
            }
            if candidates.is_empty() {
                candidates.push(ConflictCandidate {
                    target: term.target.clone(),
                    occurrences: 1,
                    chapters: vec![term.first_chapter],
                    evidence: Vec::new(),
                });
            }
            result.push(TermConflict {
                source: source.clone(),
                current_target: term.target.clone(),
                policy: term.policy,
                manual_target: term.manual_target.clone(),
                unresolved_events: events
                    .iter()
                    .filter(|(value, _, resolved)| value == &source && !resolved)
                    .count(),
                resolved_events: events
                    .iter()
                    .filter(|(value, _, resolved)| value == &source && *resolved)
                    .count(),
                candidates,
            });
        }
        Ok(result)
    }

    /// 返回与给定原文相关的术语（原文或唯一别名命中）。
    pub fn relevant(&self, source_text: &str) -> Result<Vec<Term>, String> {
        Ok(relevant_terms(&self.list()?, source_text))
    }

    /// 列出同一别名对应多个术语的冲突。
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

    /// 记录一个待补做的抽取任务（同章节同批次会被替换）。
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

    /// 列出全部待补做的抽取任务。
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

    /// 标记某个抽取任务已完成并移除。
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
