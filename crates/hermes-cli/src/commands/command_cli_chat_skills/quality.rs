{
            println!("Skill quality scorecard");
            println!("======================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            #[derive(Debug)]
            struct SkillQualityRow {
                name: String,
                score: i32,
                tier: &'static str,
                trust: String,
                revision: String,
                verdict: String,
                notes: Vec<String>,
            }

            let lock = load_v2_skill_lock(&skills_dir)?;
            let mut rows: Vec<SkillQualityRow> = Vec::new();
            let weak_regex = Regex::new(r"(?i)\b(todo|fixme|placeholder|stub)\b")
                .map_err(|error| AgentError::Config(format!("quality regex error: {error}")))?;

            for entry in &lock.packages {
                let path = skills_dir.join(&entry.install_path);
                let skill_md = path.join("SKILL.md");
                let mut score = 100i32;
                let mut notes = Vec::new();
                let content = match std::fs::read_to_string(&skill_md) {
                    Ok(content) => content,
                    Err(error) => {
                        score = 0;
                        notes.push(format!("unreadable_skill_md:{error}"));
                        String::new()
                    }
                };
                if !content.is_empty() {
                    let (frontmatter, _) =
                        hermes_tools::tools::skill_utils::parse_frontmatter(&content);
                    for required in ["name", "version", "description", "category"] {
                        if frontmatter.get(required).and_then(|value| value.as_str()).is_none() {
                            score -= 8;
                            notes.push(format!("missing_frontmatter:{required}"));
                        }
                    }
                    let line_count = content.lines().count();
                    if line_count < 20 {
                        score -= 10;
                        notes.push("short_skill_doc".to_string());
                    } else if line_count > 80 {
                        score += 4;
                    }
                    if weak_regex.is_match(&content) {
                        score -= 8;
                        notes.push("contains_placeholder_markers".to_string());
                    }
                }

                if path.join("scripts").exists() {
                    score += 6;
                } else {
                    score -= 4;
                    notes.push("no_scripts".to_string());
                }
                if path.join("examples").exists() {
                    score += 4;
                } else {
                    notes.push("no_examples".to_string());
                }
                if path.join("templates").exists() {
                    score += 3;
                }
                if path.join("tests").exists() {
                    score += 4;
                }
                if entry.quarantine.is_some() {
                    score = 0;
                    notes.push("quarantined".to_string());
                }
                if entry.manifest.scan.verdict != hermes_skills::SkillScanVerdictV2::Clean {
                    notes.push(format!(
                        "scan_verdict:{}",
                        entry.manifest.scan.verdict.as_str()
                    ));
                }

                score = score.clamp(0, 100);
                let tier = if score >= 85 {
                    "excellent"
                } else if score >= 70 {
                    "good"
                } else if score >= 55 {
                    "watch"
                } else {
                    "fallback"
                };
                rows.push(SkillQualityRow {
                    name: entry.manifest.identity.name.clone(),
                    score,
                    tier,
                    trust: skill_package_trust_label(&entry.manifest),
                    revision: entry.manifest.identity.immutable_source_revision.clone(),
                    verdict: entry.manifest.scan.verdict.as_str().to_string(),
                    notes,
                });
            }

            if rows.is_empty() {
                println!("No v2 verified skills installed.");
                return Ok(());
            }
            rows.sort_by(|left, right| {
                right
                    .score
                    .cmp(&left.score)
                    .then_with(|| left.name.cmp(&right.name))
            });
            println!(
                "{:20} {:>5} {:>10} {:18} {:14} {:10}  notes",
                "skill", "score", "tier", "trust", "revision", "verdict"
            );
            println!("{}", "-".repeat(126));
            for row in &rows {
                let notes = if row.notes.is_empty() {
                    "-".to_string()
                } else {
                    row.notes.join(",")
                };
                println!(
                    "{:20} {:>5} {:>10} {:18} {:14} {:10}  {}",
                    row.name,
                    row.score,
                    row.tier,
                    row.trust,
                    row.revision,
                    row.verdict,
                    notes
                );
            }
        }
