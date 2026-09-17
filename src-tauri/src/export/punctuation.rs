//! 中文标点规范化：把译文中的英文标点替换为全角标点。

/// 规范化中文标点。
///
/// 只做保守替换：代码块（反引号包裹）、URL 原样保留；连续三个以上句点转省略号，
/// 双连字符转破折号；替换前会去掉多余空格。
pub fn normalize_chinese_punctuation(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`' {
            let end = chars[index + 1..]
                .iter()
                .position(|character| *character == '`')
                .map(|offset| index + offset + 2)
                .unwrap_or(chars.len());
            output.extend(chars[index..end].iter());
            index = end;
            continue;
        }
        if starts_url(&chars, index) {
            while index < chars.len() && !chars[index].is_whitespace() {
                output.push(chars[index]);
                index += 1;
            }
            continue;
        }
        if chars[index] == '.' {
            let start = index;
            while index < chars.len() && chars[index] == '.' {
                index += 1;
            }
            if index - start >= 3 {
                trim_trailing_whitespace(&mut output);
                output.push_str("……");
                continue;
            }
            for _ in start..index {
                output.push('.');
            }
            continue;
        }
        if chars[index] == '-' && chars.get(index + 1) == Some(&'-') {
            trim_trailing_whitespace(&mut output);
            output.push_str("——");
            index += 2;
            continue;
        }
        let replacement = match chars[index] {
            ',' => Some('，'),
            ';' => Some('；'),
            ':' => Some('：'),
            '!' => Some('！'),
            '?' => Some('？'),
            _ => None,
        };
        if let Some(replacement) = replacement {
            trim_trailing_whitespace(&mut output);
            output.push(replacement);
        } else {
            output.push(chars[index]);
        }
        index += 1;
    }
    output
}

fn starts_url(chars: &[char], index: usize) -> bool {
    let remaining = chars[index..].iter().collect::<String>();
    remaining.starts_with("http://")
        || remaining.starts_with("https://")
        || remaining.starts_with("www.")
}

fn trim_trailing_whitespace(value: &mut String) {
    while value.ends_with(char::is_whitespace) {
        value.pop();
    }
}
