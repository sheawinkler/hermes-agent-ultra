//! Automation Blueprints for cron jobs.
//!
//! A blueprint is a typed, parameterized template that fills into a normal
//! [`CronJob`]. The scheduler remains the single job engine.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlueprintSlotKind {
    Time,
    Enum,
    Text,
    Weekdays,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintSlot {
    pub name: &'static str,
    pub kind: BlueprintSlotKind,
    pub label: &'static str,
    pub default: Option<&'static str>,
    pub options: &'static [&'static str],
    pub optional: bool,
    pub help: &'static str,
    pub strict: bool,
}

impl BlueprintSlot {
    const fn time(name: &'static str, label: &'static str, default: &'static str) -> Self {
        Self {
            name,
            kind: BlueprintSlotKind::Time,
            label,
            default: Some(default),
            options: &[],
            optional: false,
            help: "24h local time, e.g. 08:00",
            strict: true,
        }
    }

    const fn text(name: &'static str, label: &'static str, default: &'static str) -> Self {
        Self {
            name,
            kind: BlueprintSlotKind::Text,
            label,
            default: Some(default),
            options: &[],
            optional: false,
            help: "",
            strict: true,
        }
    }

    const fn enum_slot(
        name: &'static str,
        label: &'static str,
        default: &'static str,
        options: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            kind: BlueprintSlotKind::Enum,
            label,
            default: Some(default),
            options,
            optional: false,
            help: "",
            strict: true,
        }
    }

    const fn weekdays(name: &'static str, label: &'static str, default: &'static str) -> Self {
        Self {
            name,
            kind: BlueprintSlotKind::Weekdays,
            label,
            default: Some(default),
            options: &["everyday", "weekdays", "weekends"],
            optional: false,
            help: "everyday, weekdays, or weekends",
            strict: true,
        }
    }

    const fn deliver() -> Self {
        Self {
            name: "deliver",
            kind: BlueprintSlotKind::Enum,
            label: "Where to deliver?",
            default: Some("origin"),
            options: &[
                "origin",
                "local",
                "telegram",
                "discord",
                "slack",
                "email",
                "whatsapp",
                "signal",
                "matrix",
                "ntfy",
            ],
            optional: false,
            help: "origin returns to the caller; local logs only; platform names route through configured gateways",
            strict: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationBlueprint {
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub schedule_template: &'static str,
    pub prompt_template: &'static str,
    pub slots: Vec<BlueprintSlot>,
    pub deliver_default: &'static str,
    pub skills: &'static [&'static str],
    pub tags: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintJobSpec {
    pub key: String,
    pub title: String,
    pub schedule: String,
    pub prompt: String,
    pub deliver: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlueprintCommandAction {
    Catalog(String),
    Detail(String),
    Filled(BlueprintJobSpec),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlueprintFillError {
    UnknownBlueprint(String),
    UnknownSlot {
        blueprint: String,
        slot: String,
    },
    MissingSlot {
        blueprint: String,
        slot: String,
    },
    InvalidSlot {
        blueprint: String,
        slot: String,
        reason: String,
    },
    Parse(String),
}

impl std::fmt::Display for BlueprintFillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownBlueprint(key) => write!(f, "no automation blueprint named `{key}`"),
            Self::UnknownSlot { blueprint, slot } => {
                write!(f, "`{blueprint}` has no slot named `{slot}`")
            }
            Self::MissingSlot { blueprint, slot } => {
                write!(f, "`{blueprint}` needs a value for `{slot}`")
            }
            Self::InvalidSlot {
                blueprint,
                slot,
                reason,
            } => write!(f, "`{blueprint}` slot `{slot}` is invalid: {reason}"),
            Self::Parse(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for BlueprintFillError {}

pub fn catalog() -> Vec<AutomationBlueprint> {
    vec![
        AutomationBlueprint {
            key: "morning-brief",
            title: "Morning briefing",
            description: "Daily briefing with calendar, weather, and urgent items.",
            category: "daily",
            schedule_template: "{minute} {hour} * * *",
            prompt_template: "Produce a concise morning briefing: today's calendar, local weather, and urgent items. Keep it short and scannable. If connected data is unavailable, give the date and say what source is missing.",
            slots: vec![BlueprintSlot::time("time", "What time?", "08:00"), BlueprintSlot::deliver()],
            deliver_default: "origin",
            skills: &[],
            tags: &["daily", "briefing"],
        },
        AutomationBlueprint {
            key: "important-mail",
            title: "Important-mail monitor",
            description: "Check mail periodically and notify only on messages needing attention.",
            category: "email",
            schedule_template: "*/{interval_min} * * * *",
            prompt_template: "Check the user's inbox for new messages since the last run. Surface only mail matching: {criteria}. If nothing clears the bar, respond with [SILENT].",
            slots: vec![
                BlueprintSlot::enum_slot("interval_min", "How often?", "30", &["15", "30", "60"]),
                BlueprintSlot::text("criteria", "Only notify me if the mail...", "needs a reply today or mentions a deadline"),
                BlueprintSlot::deliver(),
            ],
            deliver_default: "origin",
            skills: &[],
            tags: &["email", "monitor"],
        },
        AutomationBlueprint {
            key: "weekly-review",
            title: "Weekly review",
            description: "Weekly recap of accomplishments, open items, and next week.",
            category: "weekly",
            schedule_template: "{minute} {hour} * * {dow}",
            prompt_template: "Produce a weekly review: what was accomplished, open items, and next week's calendar. Keep it tight.",
            slots: vec![
                BlueprintSlot::time("time", "What time?", "18:00"),
                BlueprintSlot::enum_slot("day", "Which day?", "sunday", &["sunday", "monday", "friday", "saturday"]),
                BlueprintSlot::deliver(),
            ],
            deliver_default: "origin",
            skills: &[],
            tags: &["weekly", "review"],
        },
        AutomationBlueprint {
            key: "workday-start",
            title: "Workday start reminder",
            description: "Weekday start-of-day agenda and top priorities.",
            category: "daily",
            schedule_template: "{minute} {hour} * * 1-5",
            prompt_template: "Give the user a brief weekday start-of-day nudge: today's calendar and the 1-3 highest-priority things to focus on.",
            slots: vec![BlueprintSlot::time("time", "What time?", "09:00"), BlueprintSlot::deliver()],
            deliver_default: "origin",
            skills: &[],
            tags: &["daily", "focus"],
        },
        AutomationBlueprint {
            key: "custom-reminder",
            title: "Custom reminder",
            description: "Recurring reminder in the user's own words.",
            category: "general",
            schedule_template: "{minute} {hour} * * {dow}",
            prompt_template: "Remind the user: {what}",
            slots: vec![
                BlueprintSlot::text("what", "Remind me to...", "take a break and stretch"),
                BlueprintSlot::time("time", "What time?", "14:00"),
                BlueprintSlot::weekdays("recurrence", "Repeat on", "everyday"),
                BlueprintSlot::deliver(),
            ],
            deliver_default: "origin",
            skills: &[],
            tags: &["reminder"],
        },
        AutomationBlueprint {
            key: "news-digest",
            title: "Topic news digest",
            description: "Recurring web digest on a chosen topic.",
            category: "research",
            schedule_template: "{minute} {hour} * * {dow}",
            prompt_template: "Search the web for new items about: {topic}. Dedupe against previous runs and deliver at most {count} concise bullets with links. If nothing is new, respond with [SILENT].",
            slots: vec![
                BlueprintSlot::text("topic", "What topic?", "AI and technology"),
                BlueprintSlot::time("time", "What time?", "18:00"),
                BlueprintSlot::weekdays("recurrence", "Repeat on", "weekdays"),
                BlueprintSlot::enum_slot("count", "How many bullets?", "5", &["3", "5", "8"]),
                BlueprintSlot::deliver(),
            ],
            deliver_default: "origin",
            skills: &[],
            tags: &["digest", "research"],
        },
        AutomationBlueprint {
            key: "hydration-move",
            title: "Hydration and movement nudge",
            description: "Periodic weekday nudge to drink water, stand up, and stretch.",
            category: "wellbeing",
            schedule_template: "0 {start_hour}-{end_hour}/{interval_hours} * * 1-5",
            prompt_template: "Send a brief, friendly nudge to drink water, stand up, and stretch. Vary the wording each time.",
            slots: vec![
                BlueprintSlot::enum_slot("interval_hours", "How often?", "1", &["1", "2", "3"]),
                BlueprintSlot::enum_slot("start_hour", "Start hour", "9", &["7", "8", "9", "10"]),
                BlueprintSlot::enum_slot("end_hour", "End hour", "17", &["16", "17", "18", "19"]),
                BlueprintSlot::deliver(),
            ],
            deliver_default: "origin",
            skills: &[],
            tags: &["wellbeing", "focus"],
        },
    ]
}

pub fn get_blueprint(key: &str) -> Option<AutomationBlueprint> {
    let query = normalize_key(key);
    catalog()
        .into_iter()
        .find(|bp| normalize_key(bp.key) == query || normalize_key(bp.title) == query)
}

pub fn match_blueprint(query: &str) -> Option<AutomationBlueprint> {
    let query = normalize_key(query);
    if query.is_empty() {
        return None;
    }
    let catalog = catalog();
    if let Some(bp) = catalog
        .iter()
        .find(|bp| normalize_key(bp.key) == query || normalize_key(bp.title) == query)
    {
        return Some(bp.clone());
    }
    if let Some(bp) = catalog.iter().find(|bp| {
        normalize_key(bp.key).starts_with(&query) || normalize_key(bp.title).starts_with(&query)
    }) {
        return Some(bp.clone());
    }
    catalog.into_iter().find(|bp| {
        normalize_key(bp.key).contains(&query) || normalize_key(bp.title).contains(&query)
    })
}

pub fn render_blueprint_catalog() -> String {
    let mut out = String::from("Automation Blueprints - `/blueprint <name>` to set one up:\n");
    for bp in catalog() {
        out.push_str(&format!(
            "  - {} - {}\n    {}\n    -> {}\n",
            bp.key,
            bp.title,
            bp.description,
            blueprint_slash_command(&bp, None)
        ));
    }
    out.push_str("\nAlias: `/bp`. Pass slot values as `name=value`; quoted text is supported.");
    out
}

pub fn render_blueprint_detail(bp: &AutomationBlueprint) -> String {
    let mut out = format!("{} - {}\n\nFields:\n", bp.title, bp.description);
    for slot in &bp.slots {
        let default = slot
            .default
            .filter(|v| !v.is_empty())
            .map(|v| format!(" [default: {v}]"))
            .unwrap_or_default();
        let options = if slot.options.is_empty() {
            String::new()
        } else {
            format!(" [options: {}]", slot.options.join(", "))
        };
        out.push_str(&format!(
            "  - {}: {}{}{}\n",
            slot.name, slot.label, options, default
        ));
    }
    out.push_str("\nReady-to-edit command:\n  ");
    out.push_str(&blueprint_slash_command(bp, None));
    out
}

pub fn blueprint_slash_command(
    bp: &AutomationBlueprint,
    values: Option<&BTreeMap<String, String>>,
) -> String {
    let mut parts = vec![format!("/blueprint {}", bp.key)];
    for slot in &bp.slots {
        let value = values
            .and_then(|m| m.get(slot.name))
            .map(String::as_str)
            .or(slot.default)
            .unwrap_or_default();
        if value.is_empty() {
            continue;
        }
        parts.push(format!("{}={}", slot.name, quote_value(value)));
    }
    parts.join(" ")
}

pub fn fill_blueprint(
    bp: &AutomationBlueprint,
    supplied: &BTreeMap<String, String>,
) -> Result<BlueprintJobSpec, BlueprintFillError> {
    let allowed = bp
        .slots
        .iter()
        .map(|slot| slot.name)
        .collect::<BTreeSet<_>>();
    for key in supplied.keys() {
        if !allowed.contains(key.as_str()) {
            return Err(BlueprintFillError::UnknownSlot {
                blueprint: bp.key.to_string(),
                slot: key.clone(),
            });
        }
    }

    let mut values = BTreeMap::<String, String>::new();
    let mut derived = BTreeMap::<String, String>::new();

    for slot in &bp.slots {
        let value = supplied
            .get(slot.name)
            .map(String::as_str)
            .or(slot.default)
            .map(str::trim)
            .unwrap_or_default();
        if value.is_empty() {
            if slot.optional {
                continue;
            }
            return Err(BlueprintFillError::MissingSlot {
                blueprint: bp.key.to_string(),
                slot: slot.name.to_string(),
            });
        }

        match slot.kind {
            BlueprintSlotKind::Time => {
                let (hour, minute) =
                    parse_time(value).map_err(|reason| BlueprintFillError::InvalidSlot {
                        blueprint: bp.key.to_string(),
                        slot: slot.name.to_string(),
                        reason,
                    })?;
                derived.insert("hour".to_string(), hour.to_string());
                derived.insert("minute".to_string(), minute.to_string());
            }
            BlueprintSlotKind::Enum => {
                if slot.strict && !slot.options.contains(&value) {
                    return Err(BlueprintFillError::InvalidSlot {
                        blueprint: bp.key.to_string(),
                        slot: slot.name.to_string(),
                        reason: format!("expected one of {}", slot.options.join(", ")),
                    });
                }
                if slot.name == "day" {
                    let dow = weekday_name_to_cron(value).map_err(|reason| {
                        BlueprintFillError::InvalidSlot {
                            blueprint: bp.key.to_string(),
                            slot: slot.name.to_string(),
                            reason,
                        }
                    })?;
                    derived.insert("dow".to_string(), dow.to_string());
                }
            }
            BlueprintSlotKind::Text => {}
            BlueprintSlotKind::Weekdays => {
                let dow = weekday_preset_to_cron(value).map_err(|reason| {
                    BlueprintFillError::InvalidSlot {
                        blueprint: bp.key.to_string(),
                        slot: slot.name.to_string(),
                        reason,
                    }
                })?;
                derived.insert("dow".to_string(), dow.to_string());
            }
        }
        values.insert(slot.name.to_string(), value.to_string());
    }

    let schedule_values = values.iter().chain(derived.iter());
    let schedule = replace_placeholders(bp.schedule_template, schedule_values);
    let prompt = replace_placeholders(bp.prompt_template, values.iter());
    let deliver = values
        .get("deliver")
        .map(String::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(bp.deliver_default)
        .to_string();

    Ok(BlueprintJobSpec {
        key: bp.key.to_string(),
        title: bp.title.to_string(),
        schedule,
        prompt,
        deliver,
        skills: bp.skills.iter().map(|skill| (*skill).to_string()).collect(),
    })
}

pub fn resolve_blueprint_command(args: &str) -> Result<BlueprintCommandAction, BlueprintFillError> {
    let tokens = split_shell_words(args)?;
    if tokens.is_empty() {
        return Ok(BlueprintCommandAction::Catalog(render_blueprint_catalog()));
    }

    let query = &tokens[0];
    let Some(bp) = match_blueprint(query) else {
        return Err(BlueprintFillError::UnknownBlueprint(query.clone()));
    };
    let supplied = parse_slot_values(&tokens[1..])?;
    if supplied.is_empty() {
        return Ok(BlueprintCommandAction::Detail(render_blueprint_detail(&bp)));
    }

    fill_blueprint(&bp, &supplied).map(BlueprintCommandAction::Filled)
}

fn parse_slot_values(tokens: &[String]) -> Result<BTreeMap<String, String>, BlueprintFillError> {
    let mut values = BTreeMap::new();
    let mut idx = 0usize;
    while idx < tokens.len() {
        let token = tokens[idx].trim();
        let Some((key, raw_value)) = token.split_once('=') else {
            return Err(BlueprintFillError::Parse(format!(
                "expected slot=value, got `{token}`"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(BlueprintFillError::Parse(
                "slot name cannot be empty".into(),
            ));
        }
        values.insert(key.to_string(), raw_value.trim().to_string());
        idx += 1;
    }
    Ok(values)
}

fn replace_placeholders<'a>(
    template: &str,
    values: impl Iterator<Item = (&'a String, &'a String)>,
) -> String {
    let mut out = template.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

fn parse_time(raw: &str) -> Result<(u8, u8), String> {
    let Some((hour, minute)) = raw.split_once(':') else {
        return Err("expected HH:MM".into());
    };
    let hour = hour
        .parse::<u8>()
        .map_err(|_| "hour must be 0-23".to_string())?;
    let minute = minute
        .parse::<u8>()
        .map_err(|_| "minute must be 0-59".to_string())?;
    if hour > 23 {
        return Err("hour must be 0-23".into());
    }
    if minute > 59 {
        return Err("minute must be 0-59".into());
    }
    Ok((hour, minute))
}

fn weekday_preset_to_cron(raw: &str) -> Result<&'static str, String> {
    match raw {
        "everyday" => Ok("*"),
        "weekdays" => Ok("1-5"),
        "weekends" => Ok("0,6"),
        other => Err(format!(
            "expected everyday, weekdays, or weekends; got {other}"
        )),
    }
}

fn weekday_name_to_cron(raw: &str) -> Result<&'static str, String> {
    match raw {
        "sunday" => Ok("0"),
        "monday" => Ok("1"),
        "friday" => Ok("5"),
        "saturday" => Ok("6"),
        other => Err(format!("unsupported day {other}")),
    }
}

fn quote_value(raw: &str) -> String {
    if raw.chars().any(char::is_whitespace) {
        format!("\"{}\"", raw.replace('"', "\\\""))
    } else {
        raw.to_string()
    }
}

fn normalize_key(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn split_shell_words(raw: &str) -> Result<Vec<String>, BlueprintFillError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;

    for ch in raw.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }

    if escape {
        current.push('\\');
    }
    if quote.is_some() {
        return Err(BlueprintFillError::Parse(
            "unterminated quoted value".into(),
        ));
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_unique_keys() {
        let mut seen = BTreeSet::new();
        for bp in catalog() {
            assert!(seen.insert(bp.key), "duplicate key {}", bp.key);
            assert!(get_blueprint(bp.key).is_some());
        }
    }

    #[test]
    fn morning_brief_defaults_fill_to_cron_job_spec() {
        let bp = get_blueprint("morning-brief").unwrap();
        let spec = fill_blueprint(&bp, &BTreeMap::new()).unwrap();
        assert_eq!(spec.schedule, "0 8 * * *");
        assert!(spec.prompt.contains("morning briefing"));
        assert_eq!(spec.deliver, "origin");
    }

    #[test]
    fn custom_reminder_quoted_text_and_weekdays_fill() {
        let action = resolve_blueprint_command(
            "custom-reminder what=\"drink water\" time=10:15 recurrence=weekdays deliver=local",
        )
        .unwrap();
        let BlueprintCommandAction::Filled(spec) = action else {
            panic!("expected filled spec");
        };
        assert_eq!(spec.schedule, "15 10 * * 1-5");
        assert_eq!(spec.prompt, "Remind the user: drink water");
        assert_eq!(spec.deliver, "local");
    }

    #[test]
    fn invalid_slot_names_are_rejected() {
        let err =
            resolve_blueprint_command("morning-brief tiem=08:00").expect_err("typo should fail");
        assert!(err.to_string().contains("tiem"));
    }

    #[test]
    fn hydration_uses_hour_field_cadence() {
        let action =
            resolve_blueprint_command("hydration-move interval_hours=2 start_hour=9 end_hour=17")
                .unwrap();
        let BlueprintCommandAction::Filled(spec) = action else {
            panic!("expected filled spec");
        };
        assert_eq!(spec.schedule, "0 9-17/2 * * 1-5");
    }

    #[test]
    fn detail_and_catalog_render_commands() {
        let catalog = resolve_blueprint_command("").unwrap();
        assert!(matches!(catalog, BlueprintCommandAction::Catalog(_)));
        let detail = resolve_blueprint_command("morning").unwrap();
        let BlueprintCommandAction::Detail(text) = detail else {
            panic!("expected detail");
        };
        assert!(text.contains("/blueprint morning-brief"));
    }
}
