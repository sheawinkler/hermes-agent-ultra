{
    let trust_action = name.as_deref().unwrap_or("help");
    if extra.as_deref().is_some_and(|value| !value.trim().is_empty()) {
        return Err(AgentError::Config(
            "skills trust accepts only direct --authority-dir, --file, and --expected-sha256 arguments"
                .to_string(),
        ));
    }
    let require_file_and_fingerprint = || -> Result<(&Path, &str), AgentError> {
        let file = trust_file.as_deref().ok_or_else(|| {
            AgentError::Config(
                "Trust provisioning requires --file <absolute-local-file>".to_string(),
            )
        })?;
        if !file.is_absolute() {
            return Err(AgentError::Config(
                "Trust provisioning --file path must be absolute".to_string(),
            ));
        }
        let expected_sha256 = expected_sha256.as_deref().ok_or_else(|| {
            AgentError::Config(
                "Trust provisioning requires --expected-sha256 <64-hex>".to_string(),
            )
        })?;
        Ok((file, expected_sha256))
    };

    match trust_action {
        "help" => {
            println!("CLI-only skill trust administration");
            println!("  hermes skills trust init-local-owner-keyring --authority-dir <absolute-owner-only-dir>");
            println!("  hermes skills trust provision-remote-root --authority-dir <absolute-owner-only-dir> --file <absolute-local-root.json> --expected-sha256 <64-hex>");
            println!("  hermes skills trust rotate-remote-root --authority-dir <absolute-owner-only-dir> --file <absolute-local-signed-keyring.json> --expected-sha256 <64-hex>");
            println!("No URL, stdin, network fetch, inferred keyring root, force, or overwrite is accepted.");
            println!("Remote root provisioning is create-only; rotation requires an old-root-signed, monotonic keyring transition.");
        }
        "init-local-owner-keyring" => {
            if trust_file.is_some() || expected_sha256.is_some() {
                return Err(AgentError::Config(
                    "init-local-owner-keyring does not accept --file or --expected-sha256"
                        .to_string(),
                ));
            }
            let authority = ensure_cli_skill_authority_directory(
                &skills_dir,
                authority_dir.as_deref(),
            )?;
            initialize_empty_local_owner_keyring(&skills_dir, &authority)?;
            println!(
                "Initialized empty owner-only local public-keyring at {}.",
                authority.local_owner_keyring_file.display()
            );
        }
        "provision-remote-root" => {
            let authority = ensure_cli_skill_authority_directory(
                &skills_dir,
                authority_dir.as_deref(),
            )?;
            ensure_local_owner_keyring_exists(&skills_dir, &authority)?;
            let (source_file, expected_sha256) = require_file_and_fingerprint()?;
            let root = hermes_skills::provision_remote_trust_root_from_file(
                &skills_dir,
                source_file,
                &authority.remote_root_file,
                expected_sha256,
            )
            .map_err(|error| trust_error("Remote trust-root provisioning failed", error))?;
            println!(
                "Provisioned remote trust root {} (minimum epoch {}) at {}.",
                root.root_key_id,
                root.minimum_epoch,
                authority.remote_root_file.display()
            );
        }
        "rotate-remote-root" => {
            let authority = load_cli_skill_trust_authority_paths(
                &skills_dir,
                authority_dir.as_deref(),
            )?;
            let (source_file, expected_sha256) = require_file_and_fingerprint()?;
            hermes_skills::install_signed_remote_keyring_from_file(
                &skills_dir,
                source_file,
                expected_sha256,
                &authority.remote_trust_config(),
            )
            .map_err(|error| trust_error("Remote trust-root rotation failed", error))?;
            println!(
                "Installed old-root-authorized remote keyring transition; the protected root file was not overwritten."
            );
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown skills trust command '{other}'. Run `hermes skills trust help`."
            )));
        }
    }
}