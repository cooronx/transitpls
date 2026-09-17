//! 术语匹配：判断原文中出现的术语与别名。

use super::{Term, TermPolicy};
use std::collections::{HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

/// 返回与给定原文相关的术语。
///
/// 直接出现原文的术语总会命中；别名只有在全库范围内唯一时才注入，
/// 避免「Captain」这类歧义别名把多个术语同时带进提示词。
/// 被标记为 `NonFixed` 或 `Ignored` 的术语不会返回。
pub fn relevant_terms(terms: &[Term], source_text: &str) -> Vec<Term> {
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
    terms
        .iter()
        .enumerate()
        .filter(|(index, term)| {
            term.policy != TermPolicy::NonFixed
                && term.policy != TermPolicy::Ignored
                && (direct.contains(&term.source) || unambiguous.contains(index))
        })
        .map(|(_, term)| term.clone())
        .collect()
}

/// 判断 `needle` 是否作为独立词出现在 `haystack` 中。
///
/// CJK 术语按连续子串匹配；拉丁文字要求两侧不是字母或数字，
/// 因此 "cat" 不会命中 "concatenate"。
pub fn matches_text(haystack: &str, needle: &str) -> bool {
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

/// 术语比较前统一做 NFKC 归一化并转小写，兼容全角与大小写差异。
pub(super) fn normalize(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect()
}

fn is_cjk(value: char) -> bool {
    matches!(
        value,
        '\u{1100}'..='\u{11ff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{31f0}'..='\u{31ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{f900}'..='\u{faff}'
    )
}
