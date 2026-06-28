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
                notes: Vec<String>,
            }

            let mut rows: Vec<SkillQualityRow> = Vec::new();
            let weak_regex = Regex::new(r"(?i)\b(todo|fixme|placeholder|stub)\b")
                .map_err(|e| AgentError::Config(format!("quality regex error: {}", e)))?;
            let risky_regex = Regex::new(r"(?i)\b(rm\s+-rf|mkfs|dd\s+if=|eval\s*\(|exec\s*\()")
                .map_err(|e| AgentError::Config(format!("quality regex error: {}", e)))?;

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if !path.is_dir() || !skill_md.exists() {
                        continue;
                    }
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    let mut score = 100i32;
                    let mut notes = Vec::new();
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let (frontmatter, _) =
                        hermes_tools::tools::skill_utils::parse_frontmatter(&content);
                    for required in ["name", "version", "description", "category"] {
                        if frontmatter.get(required).and_then(|v| v.as_str()).is_none() {
                            score -= 8;
                            notes.push(format!("missing_frontmatter:{}", required));
                        }
                    }

                    let line_count = content.lines().count();
                    if line_count < 20 {
                        score -= 10;
                        notes.push("short_skill_doc".to_string());
                    } else if line_count > 80 {
                        score += 4;
                    }

                    let scripts_dir = path.join("scripts");
                    if scripts_dir.exists() {
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

                    if weak_regex.is_match(&content) {
                        score -= 8;
                        notes.push("contains_placeholder_markers".to_string());
                    }
                    if risky_regex.is_match(&content) {
                        score -= 20;
                        notes.push("contains_risky_exec_pattern".to_string());
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
                        name,
                        score,
                        tier,
                        notes,
                    });
                }
            }

            if rows.is_empty() {
                println!("No skills installed.");
                return Ok(());
            }
            rows.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
            println!("{:28} {:>5} {:>10}  notes", "skill", "score", "tier");
            println!("{}", "-".repeat(84));
            for row in &rows {
                let notes = if row.notes.is_empty() {
                    "-".to_string()
                } else {
                    row.notes.join(",")
                };
                println!(
                    "{:28} {:>5} {:>10}  {}",
                    row.name, row.score, row.tier, notes
                );
            }

            let fallback: Vec<&SkillQualityRow> =
                rows.iter().filter(|row| row.score < 55).collect();
            if !fallback.is_empty() {
                println!("\nFallback recommendations:");
                for row in fallback {
                    println!(
                        "- {}: run `hermes skills update --apply` or reinstall from a trusted registry source.",
                        row.name
                    );
                }
            } else {
                println!("\nFallback recommendations: none (all tracked skills >= watch tier).");
            }
        }
