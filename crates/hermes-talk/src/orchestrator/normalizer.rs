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

// ---------------------------------------------------------------------------
// ZipVoice-style TTS preprocessing
// Ref: https://github.com/k2-fsa/ZipVoice/blob/master/zipvoice/tokenizer/normalizer.py
// Ref: https://github.com/k2-fsa/ZipVoice/blob/master/zipvoice/tokenizer/tokenizer.py
// ---------------------------------------------------------------------------

/// Map Chinese / variant punctuation to forms TTS engines read reliably.
pub fn map_punctuations(text: &str) -> String {
    let mut text = text.to_string();
    let pairs = [
        ('，', ','),
        ('。', '.'),
        ('！', '!'),
        ('？', '?'),
        ('；', ';'),
        ('：', ':'),
        ('、', ','),
        ('‘', '\''),
        ('’', '\''),
        ('“', '"'),
        ('”', '"'),
    ];
    for (from, to) in pairs {
        text = text.replace(from, &to.to_string());
    }
    text = text.replace("⋯", "…");
    text = text.replace("···", "…");
    text = text.replace("・・・", "…");
    text = text.replace("...", "…");
    text
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SegmentLang {
    Zh,
    En,
    Other,
}

fn is_chinese_char(c: char) -> bool {
    ('\u{4e00}'..='\u{9fa5}').contains(&c)
}

fn classify_char(c: char) -> SegmentLang {
    if is_chinese_char(c) {
        SegmentLang::Zh
    } else if c.is_ascii_alphabetic() {
        SegmentLang::En
    } else {
        SegmentLang::Other
    }
}

/// Split mixed zh/en text (ZipVoice `get_segment` semantics, without <> pinyin tags).
fn segment_by_language(text: &str) -> Vec<(String, SegmentLang)> {
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let types: Vec<SegmentLang> = chars.iter().copied().map(classify_char).collect();

    let mut segments = Vec::new();
    let mut temp = String::new();
    let mut temp_lang = types[0];

    for (i, ch) in chars.iter().enumerate() {
        let lang = types[i];
        if i == 0 {
            temp.push(*ch);
            continue;
        }
        if temp_lang == SegmentLang::Other {
            temp.push(*ch);
            temp_lang = lang;
        } else if lang == temp_lang || lang == SegmentLang::Other {
            temp.push(*ch);
        } else {
            segments.push((std::mem::take(&mut temp), temp_lang));
            temp.push(*ch);
            temp_lang = lang;
        }
    }
    if !temp.is_empty() {
        segments.push((temp, temp_lang));
    }
    segments
}

fn digit_to_chinese(d: u8) -> char {
    DIGITS[d as usize]
}

fn normalize_chinese_segment(text: &str) -> String {
    use regex::Regex;
    static PERCENT_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(\d+(?:\.\d+)?)\s*%").unwrap());
    static DECIMAL_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\d+\.\d+").unwrap());

    let mut out = text.to_string();
    out = PERCENT_RE
        .replace_all(&out, |caps: &regex::Captures| {
            let num = caps.get(1).unwrap().as_str();
            if let Ok(n) = num.parse::<u64>() {
                format!("百分之{}", number_to_chinese(n))
            } else if let Some((int_part, frac)) = num.split_once('.') {
                let int_cn = int_part
                    .parse::<u64>()
                    .map(number_to_chinese)
                    .unwrap_or_else(|_| int_part.to_string());
                let frac_cn: String = frac
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .map(|c| digit_to_chinese(c as u8 - b'0'))
                    .collect();
                format!("百分之{}点{}", int_cn, frac_cn)
            } else {
                caps.get(0).unwrap().as_str().to_string()
            }
        })
        .into_owned();

    out = DECIMAL_RE
        .replace_all(&out, |caps: &regex::Captures| {
            let num = caps.get(0).unwrap().as_str();
            let Some((int_part, frac)) = num.split_once('.') else {
                return num.to_string();
            };
            let int_cn = int_part
                .parse::<u64>()
                .map(number_to_chinese)
                .unwrap_or_else(|_| int_part.to_string());
            let frac_cn: String = frac
                .chars()
                .filter(|c| c.is_ascii_digit())
                .map(|c| digit_to_chinese(c as u8 - b'0'))
                .collect();
            format!("{}点{}", int_cn, frac_cn)
        })
        .into_owned();

    normalize_chinese_numbers(&out)
}

fn english_ones(n: u64) -> &'static str {
    match n {
        0 => "zero",
        1 => "one",
        2 => "two",
        3 => "three",
        4 => "four",
        5 => "five",
        6 => "six",
        7 => "seven",
        8 => "eight",
        9 => "nine",
        10 => "ten",
        11 => "eleven",
        12 => "twelve",
        13 => "thirteen",
        14 => "fourteen",
        15 => "fifteen",
        16 => "sixteen",
        17 => "seventeen",
        18 => "eighteen",
        19 => "nineteen",
        _ => "",
    }
}

fn english_tens(n: u64) -> &'static str {
    match n {
        2 => "twenty",
        3 => "thirty",
        4 => "forty",
        5 => "fifty",
        6 => "sixty",
        7 => "seventy",
        8 => "eighty",
        9 => "ninety",
        _ => "",
    }
}

fn english_number_to_words(n: i64) -> String {
    if n < 0 {
        return format!("minus {}", english_number_to_words(-n));
    }
    let n = n as u64;
    if n < 20 {
        return english_ones(n).to_string();
    }
    if n < 100 {
        let tens = n / 10;
        let ones = n % 10;
        if ones == 0 {
            return english_tens(tens).to_string();
        }
        return format!("{} {}", english_tens(tens), english_ones(ones));
    }
    if n < 1000 {
        let hundreds = n / 100;
        let rest = n % 100;
        if rest == 0 {
            return format!("{} hundred", english_ones(hundreds));
        }
        return format!(
            "{} hundred {}",
            english_ones(hundreds),
            english_number_to_words(rest as i64)
        );
    }
    if n < 1_000_000 {
        let thousands = n / 1000;
        let rest = n % 1000;
        if rest == 0 {
            return format!("{} thousand", english_number_to_words(thousands as i64));
        }
        return format!(
            "{} thousand {}",
            english_number_to_words(thousands as i64),
            english_number_to_words(rest as i64)
        );
    }
    n.to_string()
}

fn english_ordinal(n: u64) -> String {
    let word = english_number_to_words(n as i64);
    if word.ends_with('y') && !word.ends_with("ey") {
        return format!("{}th", word.trim_end_matches('y'));
    }
    match word.as_str() {
        "one" => "first".to_string(),
        "two" => "second".to_string(),
        "three" => "third".to_string(),
        "five" => "fifth".to_string(),
        "eight" => "eighth".to_string(),
        "nine" => "ninth".to_string(),
        "twelve" => "twelfth".to_string(),
        _ if word.ends_with('e') => format!("{}nth", word),
        _ => format!("{}th", word),
    }
}

fn expand_english_abbreviations(text: &str) -> String {
    const ABBREVS: [(&str, &str); 20] = [
        ("mrs", "misess"),
        ("mr", "mister"),
        ("dr", "doctor"),
        ("st", "saint"),
        ("co", "company"),
        ("jr", "junior"),
        ("maj", "major"),
        ("gen", "general"),
        ("drs", "doctors"),
        ("rev", "reverend"),
        ("lt", "lieutenant"),
        ("hon", "honorable"),
        ("sgt", "sergeant"),
        ("capt", "captain"),
        ("esq", "esquire"),
        ("ltd", "limited"),
        ("col", "colonel"),
        ("ft", "fort"),
        ("etc", "et cetera"),
        ("btw", "by the way"),
    ];
    let mut out = text.to_string();
    for (abbr, expansion) in ABBREVS {
        let re = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(abbr))).unwrap();
        out = re.replace_all(&out, expansion).into_owned();
    }
    out
}

fn normalize_english_segment(text: &str) -> String {
    use regex::Regex;
    static COMMA_NUM: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b(\d{1,3}(?:,\d{3})+)\b").unwrap());
    static ORDINAL: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b(\d+)(st|nd|rd|th)\b").unwrap());
    static DECIMAL: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b(\d+)\.(\d+)\b").unwrap());
    static PERCENT: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b(\d+(?:\.\d+)?)%\b").unwrap());
    static PLAIN: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\b(\d+)\b").unwrap());

    let mut out = expand_english_abbreviations(text);
    out = COMMA_NUM
        .replace_all(&out, |caps: &regex::Captures| {
            let raw = caps.get(1).unwrap().as_str().replace(',', "");
            raw.parse::<u64>()
                .map(|n| english_number_to_words(n as i64))
                .unwrap_or_else(|_| caps.get(0).unwrap().as_str().to_string())
        })
        .into_owned();
    out = ORDINAL
        .replace_all(&out, |caps: &regex::Captures| {
            let n: u64 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            english_ordinal(n)
        })
        .into_owned();
    out = PERCENT
        .replace_all(&out, |caps: &regex::Captures| {
            let num = caps.get(1).unwrap().as_str();
            if let Ok(n) = num.parse::<u64>() {
                format!("{} percent", english_number_to_words(n as i64))
            } else {
                format!("{} percent", num)
            }
        })
        .into_owned();
    out = DECIMAL
        .replace_all(&out, |caps: &regex::Captures| {
            let int_part: u64 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let frac = caps.get(2).unwrap().as_str();
            let frac_words: Vec<&str> = frac
                .chars()
                .filter(|c| c.is_ascii_digit())
                .map(|c| english_ones((c as u8 - b'0') as u64))
                .collect();
            format!(
                "{} point {}",
                english_number_to_words(int_part as i64),
                frac_words.join(" ")
            )
        })
        .into_owned();
    out = PLAIN
        .replace_all(&out, |caps: &regex::Captures| {
            let n: u64 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            english_number_to_words(n as i64)
        })
        .into_owned();
    out
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Full TTS text preprocessor (ZipVoice EmiliaTokenizer text path, without phoneme G2P).
pub fn preprocess_tts_text(text: &str) -> String {
    let mapped = map_punctuations(text);
    let segments = segment_by_language(&mapped);
    let mut out = String::with_capacity(mapped.len() + 16);
    for (seg, lang) in segments {
        let normalized = match lang {
            SegmentLang::Zh => normalize_chinese_segment(&seg),
            SegmentLang::En => normalize_english_segment(&seg),
            SegmentLang::Other => seg,
        };
        out.push_str(&normalized);
    }
    collapse_whitespace(&normalize_quotes(&out))
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
    fn test_map_punctuations() {
        assert_eq!(map_punctuations("你好，世界。"), "你好,世界.");
        assert_eq!(map_punctuations("真的吗？"), "真的吗?");
    }

    #[test]
    fn test_preprocess_chinese_numbers_and_percent() {
        let out = preprocess_tts_text("温度25度，湿度50%");
        assert!(out.contains("二十五"));
        assert!(out.contains("百分之五十"));
    }

    #[test]
    fn test_preprocess_mixed_zipvoice_example() {
        let out = preprocess_tts_text("我们是5年小米人,是吗? Yes I think so!");
        assert!(out.contains("五年"));
        assert!(out.contains("Yes I think so"));
    }

    #[test]
    fn test_preprocess_english_abbrev_and_number() {
        let out = preprocess_tts_text("mr king, 5 years");
        assert!(out.contains("mister"));
        assert!(out.contains("five"));
    }

    #[test]
    fn test_preprocess_zipvoice_main_example() {
        let text = "我们是5年小米人,是吗? Yes I think so! \
            mr king, 5 years, from 2019 to 2024.\
            霍...啦啦啦超过90%的人...?!9204";
        let out = preprocess_tts_text(text);
        assert!(out.contains("五年"), "expected cn2an-style year: {out}");
        assert!(
            out.contains("Yes I think so"),
            "expected english segment: {out}"
        );
        assert!(out.contains("mister"), "expected mr expansion: {out}");
        assert!(out.contains("five"), "expected digit expansion: {out}");
        assert!(
            out.contains("百分之九十"),
            "expected percent expansion: {out}"
        );
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
