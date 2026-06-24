//! Encode wake phrases to sherpa-onnx KWS keyword lines (Rust port of
//! [sherpa-onnx/scripts/text2token.py](https://github.com/k2-fsa/sherpa-onnx/blob/master/scripts/text2token.py)).

use std::collections::{HashMap, HashSet};

use pinyin::ToPinyin;
use tracing::warn;

use crate::config::WakeConfig;
use crate::error::{DemoError, Result};

const INITIALS: &[&str] = &[
    "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "r",
    "z", "c", "s", "y", "w",
];

struct Encoder {
    token_table: HashSet<String>,
    lexicon: HashMap<String, Vec<String>>,
    tokens_type: String,
}

/// Encode all wake phrases into newline-separated keyword lines for `keywords_buf`.
pub fn encode_wake_phrases(cfg: &WakeConfig) -> Result<String> {
    let phrases = cfg.effective_phrases();
    if phrases.is_empty() {
        return Err(DemoError::Config(
            "wake.phrases is empty; add at least one phrase".into(),
        ));
    }

    let enc = Encoder::new(cfg)?;
    let mut out_lines = Vec::new();

    for phrase in phrases {
        let tag = phrase.replace(' ', "_");
        let line = format!(
            "{phrase} :{boost} #{thresh} @{tag}",
            boost = cfg.boost_score,
            thresh = cfg.trigger_threshold,
        );
        let (text, extras) = split_text_and_extras(&line);
        let tokens = enc.encode_text(&text)?;
        let mut parts = tokens;
        parts.extend(extras);
        out_lines.push(parts.join(" "));
    }

    if out_lines.is_empty() {
        return Err(DemoError::Config(
            "no wake phrase could be encoded; check phrases and tokens.txt".into(),
        ));
    }
    Ok(out_lines.join("\n") + "\n")
}

impl Encoder {
    fn new(cfg: &WakeConfig) -> Result<Self> {
        let token_table = load_token_table(&cfg.tokens)?;
        if cfg.tokens_type == "bpe" || cfg.tokens_type == "cjkchar+bpe" {
            return Err(DemoError::Config(
                "wake.tokens_type bpe/cjkchar+bpe is not supported in-process; \
                 use phone+ppinyin for the zh-en KWS model (default)"
                    .into(),
            ));
        }

        let lexicon = if cfg.tokens_type == "phone+ppinyin" {
            load_lexicon(&cfg.lexicon)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            token_table,
            lexicon,
            tokens_type: cfg.tokens_type.clone(),
        })
    }

    fn encode_text(&self, text: &str) -> Result<Vec<String>> {
        let pieces = match self.tokens_type.as_str() {
            "cjkchar" => text.chars().map(|c| c.to_string()).collect(),
            "ppinyin" | "fpinyin" => self.encode_pinyin(text, self.tokens_type == "fpinyin")?,
            "phone+ppinyin" => self.encode_phone_ppinyin(text)?,
            other => {
                return Err(DemoError::Config(format!(
                    "unsupported wake.tokens_type: {other}"
                )));
            }
        };
        self.map_to_vocab(pieces, text)
    }

    fn encode_pinyin(&self, text: &str, full: bool) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for ch in text.chars() {
            if let Some(py) = ch.to_pinyin() {
                let syllable = py.with_tone().to_string();
                if full {
                    out.push(syllable);
                } else {
                    out.extend(split_ppinyin(&syllable));
                }
            } else if !ch.is_whitespace() {
                out.push(ch.to_string());
            }
        }
        Ok(out)
    }

    fn encode_phone_ppinyin(&self, text: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for word in text.split_whitespace() {
            if let Some(phones) = self.lexicon.get(word) {
                out.extend(phones.clone());
            } else if is_cjk_word(word) {
                out.extend(self.encode_pinyin(word, false)?);
            } else {
                warn!(
                    word,
                    text, "word not in lexicon and not CJK; skipping phrase"
                );
                return Err(DemoError::Config(format!(
                    "cannot encode {word:?} in {text:?} (not in lexicon / not CJK)"
                )));
            }
        }
        Ok(out)
    }

    fn map_to_vocab(&self, pieces: Vec<String>, source: &str) -> Result<Vec<String>> {
        let mut mapped = Vec::new();
        for p in pieces {
            if self.token_table.contains(&p) {
                mapped.push(p);
            } else {
                return Err(DemoError::Config(format!(
                    "token {p:?} not in {}; cannot encode phrase {source:?}",
                    "tokens.txt"
                )));
            }
        }
        Ok(mapped)
    }
}

fn split_text_and_extras(line: &str) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut extras = Vec::new();
    for tok in line.split_whitespace() {
        if tok.starts_with(':') || tok.starts_with('#') || tok.starts_with('@') {
            extras.push(tok.to_string());
        } else {
            text.push(tok);
        }
    }
    (text.join(" "), extras)
}

fn split_ppinyin(syllable: &str) -> Vec<String> {
    let mut initial = String::new();
    let mut rest = syllable;
    for init in INITIALS {
        if rest.starts_with(init) {
            initial = init.to_string();
            rest = &rest[init.len()..];
            break;
        }
    }
    let mut out = Vec::new();
    if !initial.is_empty() {
        out.push(initial);
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    if out.is_empty() && !syllable.is_empty() {
        out.push(syllable.to_string());
    }
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF)
}

fn is_cjk_word(word: &str) -> bool {
    !word.is_empty() && word.chars().all(is_cjk)
}

fn load_token_table(path: &str) -> Result<HashSet<String>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| DemoError::Config(format!("read tokens {}: {e}", path)))?;
    let mut set = HashSet::new();
    for line in content.lines() {
        let token = line.split_whitespace().next().unwrap_or("");
        if !token.is_empty() {
            set.insert(token.to_string());
        }
    }
    Ok(set)
}

fn load_lexicon(path: &str) -> Result<HashMap<String, Vec<String>>> {
    if path.is_empty() {
        return Err(DemoError::Config(
            "wake.lexicon required for phone+ppinyin".into(),
        ));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| DemoError::Config(format!("read lexicon {}: {e}", path)))?;
    let mut map = HashMap::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let word = match parts.next() {
            Some(w) => w.to_string(),
            None => continue,
        };
        let phones: Vec<String> = parts.map(|s| s.to_string()).collect();
        if !phones.is_empty() {
            map.insert(word, phones);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ppinyin_syllables() {
        assert_eq!(split_ppinyin("zhōng"), vec!["zh", "ōng"]);
        assert_eq!(split_ppinyin("shì"), vec!["sh", "ì"]);
    }

    #[test]
    fn split_text_and_extras_parses_scores() {
        let (text, extras) = split_text_and_extras("小智小智 :2.0 #0.35 @小智小智");
        assert_eq!(text, "小智小智");
        assert_eq!(extras, vec![":2.0", "#0.35", "@小智小智"]);
    }
}
