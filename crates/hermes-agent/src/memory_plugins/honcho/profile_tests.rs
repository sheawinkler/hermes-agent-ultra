#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{
        MemoryExportArtifact, MemoryExportCompletion, MemoryExportPage, MemoryExportSink,
        MemoryExportSinkError, MemoryExportSnapshot, MemoryPrincipalId, MemorySessionId,
        MemorySubjectDerivationKey, MemorySurface, MemoryTenantId,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[derive(Default)]
    struct TestExportSink {
        pages: Vec<MemoryExportPage>,
        completion: Option<MemoryExportCompletion>,
    }

    impl MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: MemoryExportSnapshot,
        ) -> Result<(), MemoryExportSinkError> {
            Ok(())
        }

        fn append_page(&mut self, page: MemoryExportPage) -> Result<(), MemoryExportSinkError> {
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            completion: MemoryExportCompletion,
        ) -> Result<(), MemoryExportSinkError> {
            self.completion = Some(completion);
            Ok(())
        }

        fn abort(&mut self) {}

        fn commit(self: Box<Self>) -> Result<MemoryExportArtifact, MemoryExportSinkError> {
            Err(MemoryExportSinkError::Incomplete)
        }
    }

    fn subject(principal: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::new(
            MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new(principal).expect("principal"),
            None,
            MemorySessionId::new("session-a").expect("session"),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .expect("subject");
        let filter = subject.storage_key(&MemorySubjectDerivationKey::from_bytes([9; 32]));
        (subject, filter)
    }

    fn config(base_url: String) -> HonchoConfig {
        HonchoConfig {
            api_key: String::new(),
            base_url,
            enabled: true,
            recall_mode: "hybrid".to_string(),
            context_tokens: Some(800),
            timeout_secs: 2.0,
            endpoints: HashMap::new(),
            host_had_explicit_api_key: false,
            host: HOST.to_string(),
            config_path: HonchoConfig::default_config_path(),
            oauth: None,
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let count = stream.read(&mut chunk).expect("read request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8(bytes).expect("utf8 request")
    }

    fn server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("address");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                sender.send(read_request(&mut stream)).expect("record request");
                let reason = if (200..300).contains(&status) { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("respond");
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn plugin(
        config: HonchoConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> HonchoMemoryPlugin {
        let plugin = HonchoMemoryPlugin::new();
        *plugin.config.lock() = Some(config);
        *plugin.bound_subject.lock() = Some(subject.clone());
        *plugin.bound_filter.lock() = Some(filter.clone());
        plugin
    }

    #[test]
    fn subject_peer_is_opaque_and_tool_identity_override_is_rejected() {
        let (alice, alice_filter) = subject("alice");
        let (_bob, bob_filter) = subject("bob");
        assert_ne!(alice_filter, bob_filter);
        let (base, requests) = server(vec![(200, r#"{"results":[]}"#)]);
        let plugin = plugin(config(base), &alice, &alice_filter);

        let rejected = plugin.handle_tool_call(
            &alice,
            &alice_filter,
            "honcho_search",
            &json!({"query":"x", "runtime_user_id":"victim"}),
        );
        assert!(rejected.contains("cannot be overridden"));
        let result = plugin.handle_tool_call(
            &alice,
            &alice_filter,
            "honcho_search",
            &json!({"query":"x"}),
        );
        assert!(result.contains("results"));
        let request = requests.recv_timeout(Duration::from_secs(2)).expect("scoped request");
        assert!(request.contains(alice_filter.as_str()));
        assert!(!request.contains("alice"));
        assert!(!request.contains("victim"));
    }

    #[test]
    fn bound_subject_rejects_foreign_subject_with_reused_filter() {
        let (alice, filter) = subject("alice");
        let (bob, _bob_filter) = subject("bob");
        let plugin = plugin(config("http://127.0.0.1:9".to_string()), &alice, &filter);
        let result = plugin.handle_tool_call(&bob, &filter, "honcho_search", &json!({"query":"x"}));
        assert!(result.contains("does not match initialized subject"));
    }

    #[test]
    fn governance_requires_a_stable_subject_inventory() {
        let (subject, filter) = subject("alice");
        let plugin = plugin(
            config("http://127.0.0.1:9".to_string()),
            &subject,
            &filter,
        );
        let actor = MemorySubjectDerivationKey::from_bytes([8; 32]).actor_for_subject(&subject);
        assert!(matches!(
            plugin.forget_subject(&subject, &filter, &actor),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Forget,
                ..
            })
        ));
        let policy = MemoryRetentionPolicy::new(None, Some(1)).expect("policy");
        assert!(matches!(
            plugin.retention_sweep(&subject, &filter, &policy, &actor),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::RetentionSweep,
                ..
            })
        ));
        assert!(matches!(
            plugin.verify_subject_absent(&subject, &filter),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Readback,
                ..
            })
        ));
    }

    #[test]
    fn export_streams_a_stable_subject_snapshot() {
        let (subject, filter) = subject("alice");
        let (base, requests) = server(vec![(
            200,
            r#"{"snapshot":"snapshot-a","revision":"revision-a","results":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#,
        )]);
        let plugin = plugin(config(base), &subject, &filter);
        let actor = MemorySubjectDerivationKey::from_bytes([8; 32]).actor_for_subject(&subject);
        let mut sink = TestExportSink::default();
        assert_eq!(
            plugin
                .export_subject(&subject, &filter, &actor, &mut sink)
                .expect("export")
                .exported_records,
            3
        );
        assert_eq!(sink.pages.len(), 1);
        assert!(sink.completion.as_ref().expect("completion").exhausted);
        assert!(requests
            .recv_timeout(Duration::from_secs(2))
            .expect("request")
            .contains(filter.as_str()));
    }
}
