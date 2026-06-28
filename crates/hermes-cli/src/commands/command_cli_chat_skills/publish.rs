{
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills publish <name>".into(),
                )
            })?;
            let skill_path = skills_dir.join(&skill_name);
            if !skill_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "Skill '{}' not found.",
                    skill_name
                )));
            }
            println!("Publishing skill '{}' to Skills Hub...", skill_name);
            println!("  Source: {}", skill_path.display());

            let skill_md = skill_path.join("SKILL.md");
            if !skill_md.exists() {
                println!("  ✗ Missing SKILL.md — required for publishing.");
                return Ok(());
            }

            let content = std::fs::read_to_string(&skill_md)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let (frontmatter, _body) =
                hermes_tools::tools::skill_utils::parse_frontmatter(&content);

            let fm_name = frontmatter.get("name").and_then(|v| v.as_str());
            let fm_version = frontmatter.get("version").and_then(|v| v.as_str());
            let fm_desc = frontmatter.get("description").and_then(|v| v.as_str());
            let fm_category = frontmatter.get("category").and_then(|v| v.as_str());

            if fm_name.is_none()
                || fm_version.is_none()
                || fm_desc.is_none()
                || fm_category.is_none()
            {
                println!(
                    "  ✗ SKILL.md frontmatter must include: name, version, description, category"
                );
                let mut missing = Vec::new();
                if fm_name.is_none() {
                    missing.push("name");
                }
                if fm_version.is_none() {
                    missing.push("version");
                }
                if fm_desc.is_none() {
                    missing.push("description");
                }
                if fm_category.is_none() {
                    missing.push("category");
                }
                println!("    Missing: {}", missing.join(", "));
                return Ok(());
            }

            let publish_name = fm_name.unwrap();
            let publish_version = fm_version.unwrap();
            let publish_desc = fm_desc.unwrap();
            let publish_category = fm_category.unwrap();
            println!(
                "  ✓ name={}, version={}, category={}",
                publish_name, publish_version, publish_category
            );
            println!("  ✓ description: {}", publish_desc);

            // Package skill directory into a tarball in memory
            let mut tar_buf = Vec::new();
            {
                let enc =
                    flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
                let mut tar_builder = tar::Builder::new(enc);
                tar_builder
                    .append_dir_all(&skill_name, &skill_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
                tar_builder
                    .finish()
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;
            }
            println!("  ✓ Packaged {} bytes", tar_buf.len());

            // Read hub token
            let token_path = hermes_config::hermes_home().join("hub_token");
            if !token_path.exists() {
                println!("  ✗ No hub token found at {}", token_path.display());
                println!("    Run `hermes login hub` to authenticate with Skills Hub.");
                return Ok(());
            }
            let hub_token = std::fs::read_to_string(&token_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Token read error: {}", e)))?
                .trim()
                .to_string();

            // Build metadata JSON
            let metadata = serde_json::json!({
                "name": publish_name,
                "version": publish_version,
                "description": publish_desc,
                "category": publish_category,
            });

            // Upload to Skills Hub API via multipart
            let tarball_part = reqwest::multipart::Part::bytes(tar_buf)
                .file_name(format!("{}-{}.tar.gz", publish_name, publish_version))
                .mime_str("application/gzip")
                .unwrap();
            let metadata_part = reqwest::multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .unwrap();
            let form = reqwest::multipart::Form::new()
                .part("tarball", tarball_part)
                .part("metadata", metadata_part);

            println!("  Uploading to Skills Hub...");
            match reqwest::Client::new()
                .post("https://agentskills.io/api/v1/skills")
                .bearer_auth(&hub_token)
                .multipart(form)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let url = format!("https://agentskills.io/skills/{}", publish_name);
                    println!("  ✓ Published successfully!");
                    println!("  URL: {}", url);
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::CONFLICT => {
                    println!(
                        "  ✗ Version {} already exists on Skills Hub.",
                        publish_version
                    );
                    println!("    Bump the version in SKILL.md frontmatter and try again.");
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    println!("  ✗ Unauthorized. Hub token may be expired.");
                    println!("    Run `hermes login hub` to re-authenticate.");
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    println!("  ✗ Upload failed (HTTP {}): {}", status, body);
                }
                Err(e) => {
                    println!("  ✗ Could not reach Skills Hub: {}", e);
                }
            }
        }
