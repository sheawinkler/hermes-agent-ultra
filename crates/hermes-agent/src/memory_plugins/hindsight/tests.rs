#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    #[derive(Default)]
    struct TestExportSink;

    impl hermes_core::MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: hermes_core::MemoryExportSnapshot,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn append_page(
            &mut self,
            _page: hermes_core::MemoryExportPage,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn finish_provider(
            &mut self,
            _completion: hermes_core::MemoryExportCompletion,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn abort(&mut self) {}

        fn commit(
            self: Box<Self>,
        ) -> Result<hermes_core::MemoryExportArtifact, hermes_core::MemoryExportSinkError> {
            Err(hermes_core::MemoryExportSinkError::Incomplete)
        }
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn subject(label: &str) -> (
        MemorySubject,
        MemorySubjectStorageKey,
        MemoryAuditActor,
    ) {
        let derivation = hermes_core::MemorySubjectDerivationKey::from_bytes([17; 32]);
        let subject = MemorySubject::local_owner(
            hermes_core::MemorySurface::Local,
            format!("hindsight-{label}"),
        )
        .expect("subject");
        let key = subject.storage_key(&derivation);
        let actor = derivation.actor_for_subject(&subject);
        (subject, key, actor)
    }

    fn write_hindsight_config(hermes_home: &std::path::Path, value: &Value) {
        let path = hermes_home.join("hindsight").join("config.json");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config");
        std::fs::write(&path, serde_json::to_vec(value).expect("serialize config"))
            .expect("write config");
    }

    fn mock_hindsight_server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let (sender, receiver) = mpsc::channel();
        let join = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 16_384];
                let read = stream.read(&mut request).expect("read");
                let first = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                sender.send(first).expect("record request");
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("response");
            }
        });
        (format!("http://{address}"), receiver, join)
    }

    #[test]
    fn subject_scoped_bank_keys_are_opaque_and_isolated() {
        let (_, key_a, _) = subject("a");
        let (_, key_b, _) = subject("b");
        let bank_a = subject_scoped_bank_id("shared namespace", &key_a);
        let bank_b = subject_scoped_bank_id("shared namespace", &key_b);
        assert_ne!(bank_a, bank_b);
        assert!(bank_a.ends_with(key_a.as_str()));
        assert!(bank_b.ends_with(key_b.as_str()));
        assert!(!bank_a.contains("hindsight-a"));
        assert!(!bank_b.contains("hindsight-b"));
    }

    #[test]
    fn initialize_binds_an_opaque_subject_and_disables_async_retain() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(
            tmp.path(),
            &json!({"mode": "cloud", "api_key": "test", "bank_id": "shared"}),
        );
        let (subject, key, _) = subject("bound");
        let plugin = HindsightPlugin::new();
        plugin
            .initialize(&subject, &key, tmp.path().to_str().expect("tmp"))
            .expect("initialize");
        let config = plugin.config.lock().clone().expect("config");
        assert!(config.bank_id.ends_with(key.as_str()));
        assert!(!config.bank_id.contains("hindsight-bound"));
        assert!(!config.retain_async);
        assert_eq!(plugin.subject_key.lock().as_ref(), Some(&key));
    }

    #[test]
    fn tools_cannot_override_the_bound_subject() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(tmp.path(), &json!({"api_key": "test"}));
        let (subject_a, key_a, _) = subject("a");
        let (subject_b, key_b, _) = subject("b");
        let plugin = HindsightPlugin::new();
        plugin
            .initialize(&subject_a, &key_a, tmp.path().to_str().expect("tmp"))
            .expect("initialize");
        let response = plugin.handle_tool_call(
            &subject_b,
            &key_b,
            "hindsight_recall",
            &json!({"query": "private memory"}),
        );
        assert!(response.contains("authenticated subject"));
        for schema in plugin.get_tool_schemas() {
            assert!(schema["parameters"]["properties"].get("subject").is_none());
            assert!(schema["parameters"]["properties"].get("bank").is_none());
        }
    }

    #[test]
    fn export_forget_and_readback_use_only_the_subject_bank() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let (base, requests, server) = mock_hindsight_server(vec![
            ("200 OK", r#"{"items":[{"id":"one"},{"id":"two"}]}"#),
            ("200 OK", r#"{}"#),
            ("404 Not Found", r#"{}"#),
            ("404 Not Found", r#"{}"#),
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(
            tmp.path(),
            &json!({"mode": "cloud", "api_key": "test", "api_url": base, "bank_id": "namespace"}),
        );
        let (subject, key, actor) = subject("governance");
        let plugin = HindsightPlugin::new();
        plugin
            .initialize(&subject, &key, tmp.path().to_str().expect("tmp"))
            .expect("initialize");
        let bank = plugin
            .config
            .lock()
            .as_ref()
            .expect("config")
            .bank_id
            .clone();

        let mut sink = TestExportSink;
        assert!(matches!(
            plugin.export_subject(&subject, &key, &actor, &mut sink),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Export,
                ..
            })
        ));
        assert_eq!(
            plugin
                .forget_subject(&subject, &key, &actor)
                .expect("forget")
                .deleted_records,
            2
        );
        assert!(plugin
            .verify_subject_absent(&subject, &key)
            .expect("readback")
            .is_absent());
        server.join().expect("server");
        let paths = requests.try_iter().collect::<Vec<_>>();
        assert_eq!(paths.len(), 4);
        assert!(paths.iter().all(|path| path.contains(&bank)));
        assert!(paths.iter().all(|path| !path.contains("hindsight-governance")));
        assert!(paths.iter().any(|path| path.starts_with("DELETE ")));
    }

    #[test]
    fn retention_fails_closed_when_the_remote_api_cannot_apply_it_exactly() {
        let (subject, key, actor) = subject("retention");
        let error = HindsightPlugin::new()
            .retention_sweep(
                &subject,
                &key,
                &MemoryRetentionPolicy::new(Some(1), None).expect("policy"),
                &actor,
            )
            .expect_err("unsupported");
        assert!(matches!(
            error,
            MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::RetentionSweep,
                ..
            }
        ));
    }

    #[test]
    fn scoped_document_identifiers_never_embed_raw_subject_fields() {
        let (_, key, _) = subject("document");
        let id = scoped_document_id(key.as_str());
        assert!(id.starts_with(key.as_str()));
        assert!(!id.contains("hindsight-document"));
    }

    #[test]
    fn recall_body_defaults_to_observations() {
        let body = hindsight_recall_body("dark mode", "mid", 4096, &default_recall_types());
        assert_eq!(body["query"], "dark mode");
        assert_eq!(body["types"], json!(["observation"]));
    }
}
