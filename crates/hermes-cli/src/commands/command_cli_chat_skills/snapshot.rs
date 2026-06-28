{
            let sub = name.as_deref().unwrap_or("export");
            match sub {
                "export" => {
                    let output = extra.unwrap_or_else(|| {
                        format!(
                            "skills-snapshot-{}.tar.gz",
                            chrono::Utc::now().format("%Y%m%d-%H%M%S")
                        )
                    });
                    println!("Exporting skills snapshot to: {}", output);
                    if !skills_dir.exists() {
                        println!("No skills directory found.");
                        return Ok(());
                    }
                    // Create a tar.gz archive of skills directory
                    let tar_gz = std::fs::File::create(&output).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create archive: {}", e))
                    })?;
                    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
                    let mut tar = tar::Builder::new(enc);
                    tar.append_dir_all("skills", &skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to archive: {}", e))
                    })?;
                    tar.finish().map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to finalize archive: {}", e))
                    })?;
                    println!("Snapshot exported to: {}", output);
                }
                "import" => {
                    let input = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing snapshot path. Usage: hermes skills snapshot import <path>"
                                .into(),
                        )
                    })?;
                    println!("Importing skills snapshot from: {}", input);
                    let tar_gz = std::fs::File::open(&input).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to open archive: {}", e))
                    })?;
                    let dec = flate2::read::GzDecoder::new(tar_gz);
                    let mut archive = tar::Archive::new(dec);
                    std::fs::create_dir_all(&skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create skills dir: {}", e))
                    })?;
                    archive.unpack(hermes_config::hermes_home()).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to extract archive: {}", e))
                    })?;
                    println!("Snapshot imported successfully.");
                }
                _ => {
                    println!("Usage: hermes skills snapshot export|import [path]");
                }
            }
        }
