{
            let sub = name.as_deref().unwrap_or("list");
            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            let subscriptions_file = skills_dir.join("subscriptions.json");
            match sub {
                "list" => {
                    let taps = effective_skill_taps(&taps_file, &subscriptions_file);
                    if taps.is_empty() {
                        println!("No skill taps configured.");
                    } else {
                        println!("Skill taps:");
                        for tap in &taps {
                            println!("  • {}", tap);
                        }
                    }
                }
                "add" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap add <url>".into(),
                        )
                    })?;
                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    if effective_skill_taps(&taps_file, &subscriptions_file).contains(&url) {
                        println!("Tap already exists: {}", url);
                    } else {
                        taps.push(url.clone());
                        write_skill_taps(&taps_file, &taps)?;
                        println!("Added tap: {}", url);
                    }
                }
                "remove" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap remove <url>".into(),
                        )
                    })?;
                    if DEFAULT_SKILL_TAPS
                        .iter()
                        .any(|default_tap| default_tap == &url.as_str())
                    {
                        println!("Tap '{}' is a built-in default and cannot be removed.", url);
                        println!(
                            "Add custom taps with `hermes skills tap add <url>`; defaults remain active."
                        );
                        return Ok(());
                    }

                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    let before_len = taps.len();
                    taps.retain(|t| t != &url);
                    if taps.len() < before_len {
                        write_skill_taps(&taps_file, &taps)?;
                        println!("Removed tap: {}", url);
                    } else {
                        println!("Tap not found: {}", url);
                    }
                }
                _ => {
                    println!("Usage: hermes skills tap list|add|remove [url]");
                }
            }
        }
