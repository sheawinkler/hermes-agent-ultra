const DIGITS: [char; 10] = ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
const UNITS: [char; 4] = [' ', '十', '百', '千'];
const BIG_UNITS: [&str; 3] = ["", "万", "亿"];

/// Convert an integer to its Chinese reading.
/// e.g. 123 → "一百二十三", 10001 → "一万零一"
pub fn number_to_chinese(n: u64) -> String {
    if n == 0 {
        return "零".to_string();
    }
    if n < 10 {
        return DIGITS[n as usize].to_string();
    }
    if n < 20 {
        let g = n % 10;
        if g == 0 {
            return "十".to_string();
        }
        return format!("十{}", DIGITS[g as usize]);
    }

    let mut segments: Vec<u64> = Vec::new();
    let mut remaining = n;
    while remaining > 0 {
        segments.push(remaining % 10000);
        remaining /= 10000;
    }

    let mut result = String::new();
    let mut need_zero = false;

    for (i, &seg) in segments.iter().enumerate().rev() {
        if seg == 0 {
            if !result.is_empty() {
                need_zero = true;
            }
            continue;
        }

        if need_zero {
            result.push('零');
            need_zero = false;
        } else if !result.is_empty() && seg < 1000 {
            result.push('零');
        }

        result.push_str(&segment4_to_chinese(seg));
        result.push_str(BIG_UNITS[i]);
    }

    result
}

fn segment4_to_chinese(n: u64) -> String {
    debug_assert!(n < 10000);
    if n == 0 {
        return String::new();
    }

    let q = n / 1000;
    let b = (n % 1000) / 100;
    let s = (n % 100) / 10;
    let g = n % 10;

    let parts = [(q, 3usize), (b, 2), (s, 1), (g, 0)];

    let mut result = String::new();
    let mut zero_pending = false;

    for &(digit, pos) in &parts {
        if digit > 0 {
            if zero_pending {
                result.push('零');
                zero_pending = false;
            }
            result.push(DIGITS[digit as usize]);
            if pos > 0 {
                result.push(UNITS[pos as usize]);
            }
        } else if !result.is_empty() {
            zero_pending = true;
        }
    }

    result
}

/// Replace Arabic numerals in text with their Chinese readings.
/// e.g. "我有123个苹果" → "我有一百二十三个苹果"
pub fn normalize_chinese_numbers(text: &str) -> String {
    let mut result = String::with_capacity(text.len() * 2);
    let mut num_start: Option<usize> = None;

    for (char_offset, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            if num_start.is_none() {
                num_start = Some(char_offset);
            }
        } else {
            if let Some(start) = num_start {
                let num_str = &text[start..char_offset];
                if let Ok(num) = num_str.parse::<u64>() {
                    result.push_str(&number_to_chinese(num));
                } else {
                    result.push_str(num_str);
                }
                num_start = None;
            }
            result.push(ch);
        }
    }

    if let Some(start) = num_start {
        let num_str = &text[start..];
        if let Ok(num) = num_str.parse::<u64>() {
            result.push_str(&number_to_chinese(num));
        } else {
            result.push_str(num_str);
        }
    }

    result
}

/// Replace typographic/curly quotes with Chinese corner-bracket equivalents
/// that are in the Rockchip TTS dictionary.
pub fn normalize_quotes(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{201c}' => '\u{300c}', // " → 「
            '\u{201d}' => '\u{300d}', // " → 」
            '\u{2018}' => '\u{300e}', // ' → 『
            '\u{2019}' => '\u{300f}', // ' → 』
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_number_to_chinese_basic() {
        assert_eq!(number_to_chinese(0), "零");
        assert_eq!(number_to_chinese(1), "一");
        assert_eq!(number_to_chinese(5), "五");
        assert_eq!(number_to_chinese(9), "九");
        assert_eq!(number_to_chinese(10), "十");
        assert_eq!(number_to_chinese(11), "十一");
        assert_eq!(number_to_chinese(15), "十五");
        assert_eq!(number_to_chinese(19), "十九");
        assert_eq!(number_to_chinese(20), "二十");
        assert_eq!(number_to_chinese(21), "二十一");
        assert_eq!(number_to_chinese(99), "九十九");
    }

    #[test]
    fn test_number_to_chinese_hundreds() {
        assert_eq!(number_to_chinese(100), "一百");
        assert_eq!(number_to_chinese(101), "一百零一");
        assert_eq!(number_to_chinese(110), "一百一十");
        assert_eq!(number_to_chinese(111), "一百一十一");
        assert_eq!(number_to_chinese(200), "二百");
        assert_eq!(number_to_chinese(999), "九百九十九");
    }

    #[test]
    fn test_number_to_chinese_thousands() {
        assert_eq!(number_to_chinese(1000), "一千");
        assert_eq!(number_to_chinese(1001), "一千零一");
        assert_eq!(number_to_chinese(1010), "一千零一十");
        assert_eq!(number_to_chinese(1100), "一千一百");
        assert_eq!(number_to_chinese(1110), "一千一百一十");
        assert_eq!(number_to_chinese(9999), "九千九百九十九");
    }

    #[test]
    fn test_number_to_chinese_wan() {
        assert_eq!(number_to_chinese(10000), "一万");
        assert_eq!(number_to_chinese(10001), "一万零一");
        assert_eq!(number_to_chinese(10010), "一万零一十");
        assert_eq!(number_to_chinese(10100), "一万零一百");
        assert_eq!(number_to_chinese(11000), "一万一千");
        assert_eq!(number_to_chinese(11111), "一万一千一百一十一");
        assert_eq!(number_to_chinese(100000), "一十万");
        assert_eq!(number_to_chinese(100001), "一十万零一");
        assert_eq!(number_to_chinese(100100), "一十万零一百");
        assert_eq!(number_to_chinese(101000), "一十万一千");
        assert_eq!(
            number_to_chinese(99999999),
            "九千九百九十九万九千九百九十九"
        );
    }

    #[test]
    fn test_number_to_chinese_yi() {
        assert_eq!(number_to_chinese(100000000), "一亿");
        assert_eq!(number_to_chinese(100000001), "一亿零一");
        assert_eq!(number_to_chinese(100002000), "一亿零二千");
        assert_eq!(
            number_to_chinese(123456789),
            "一亿二千三百四十五万六千七百八十九"
        );
    }

    #[test]
    fn test_normalize_chinese_numbers_in_text() {
        assert_eq!(normalize_chinese_numbers("我有3个苹果"), "我有三个苹果");
        assert_eq!(normalize_chinese_numbers("第1名"), "第一名");
        assert_eq!(normalize_chinese_numbers("温度25度"), "温度二十五度");
        assert_eq!(normalize_chinese_numbers("2024年"), "二千零二十四年");
        assert_eq!(normalize_chinese_numbers("10点30分"), "十点三十分");
    }

    #[test]
    fn test_normalize_chinese_numbers_no_change() {
        assert_eq!(normalize_chinese_numbers("你好世界"), "你好世界");
        assert_eq!(normalize_chinese_numbers(""), "");
        assert_eq!(normalize_chinese_numbers("abc"), "abc");
    }

    #[test]
    fn test_normalize_quotes() {
        assert_eq!(
            normalize_quotes("\u{201c}你好\u{201d}"),
            "\u{300c}你好\u{300d}"
        );
        assert_eq!(normalize_quotes("ordinary"), "ordinary");
    }
}
