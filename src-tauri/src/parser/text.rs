//! 文本工具：编码校验、长段落切分与哈希。

use sha2::{Digest, Sha256};

/// 长文本切分。
///
/// 超过 `max_chars` 时按句末标点或换行就近断开，避免把一句话截成两段。
pub(crate) fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + max_chars).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            for index in (start..hard_end).rev() {
                if is_sentence_boundary(chars[index]) {
                    end = index + 1;
                    break;
                }
            }
        }
        if end == start {
            end = hard_end;
        }
        let chunk: String = chars[start..end].iter().collect();
        if !chunk.trim().is_empty() {
            chunks.push(chunk.trim().to_string());
        }
        start = end;
    }
    chunks
}

fn is_sentence_boundary(value: char) -> bool {
    matches!(
        value,
        '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}' | '\n'
    )
}

/// 解码 TXT 文本，允许 UTF-8 BOM，其他编码直接报错。
pub(super) fn decode_utf8(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|_| "TXT input must be UTF-8 (BOM is supported)".to_string())
}

/// 合并连续空白，用于生成稳定的段落 ID 和提示词文本。
pub(crate) fn normalize_source(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 对文本取 SHA-256，用于章节与段落 ID。
pub(super) fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
