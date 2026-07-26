use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration as StdDuration;
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

fn subject(label: &str) -> (MemorySubject, MemorySubjectStorageKey) {
    let subject = MemorySubject::local_owner(hermes_core::MemorySurface::Local, label)
        .expect("explicit local subject");
    let derivation = hermes_core::MemorySubjectDerivationKey::from_bytes([19_u8; 32]);
    let key = subject.storage_key(&derivation);
    (subject, key)
}

fn actor(subject: &MemorySubject) -> hermes_core::MemoryAuditActor {
    hermes_core::MemorySubjectDerivationKey::from_bytes([23_u8; 32]).actor_for_subject(subject)
}

fn root(key: &MemorySubjectStorageKey) -> String {
    format!("viking://user/{}/memories", key.as_str())
}

fn mock_server(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .expect("timeout");
            let mut bytes = vec![0_u8; 32 * 1024];
            let read = stream.read(&mut bytes).expect("read request");
            sender
                .send(String::from_utf8_lossy(&bytes[..read]).to_string())
                .expect("record request");
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (format!("http://{address}"), receiver)
}

fn plugin(endpoint: String, key: &MemorySubjectStorageKey) -> OpenVikingMemoryPlugin {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.state.lock().expect("state") = Some(VikingState {
        client: Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client"),
        endpoint,
        api_key: "test-key".to_string(),
        account: key.as_str().to_string(),
        user: key.as_str().to_string(),
        agent: key.as_str().to_string(),
        subject_key: key.clone(),
        session_id: key.as_str().to_string(),
        turn_count: 0,
    });
    plugin
}

#[test]
fn subject_partition_rejects_foreign_tool_uri_and_state() {
    let (alice, alice_key) = subject("alice-session");
    let (bob, bob_key) = subject("bob-session");
    let plugin = plugin("http://127.0.0.1:9".to_string(), &alice_key);

    let rejected = MemoryProviderPlugin::handle_tool_call(
        &plugin,
        &alice,
        &alice_key,
        VIKING_READ_TOOL,
        &json!({"uri": "viking://user/other/memories/private.md"}),
    );
    assert!(rejected.contains("outside the authenticated subject"));
    assert!(MemoryProviderPlugin::prefetch(&plugin, &bob, &bob_key, "project memory").is_empty());
}

#[test]
fn search_forces_authenticated_subject_root() {
    let (subject, key) = subject("search-session");
    let (endpoint, requests) = mock_server(vec![(200, "{\"result\":[]}".to_string())]);
    let plugin = plugin(endpoint, &key);

    let result = MemoryProviderPlugin::handle_tool_call(
        &plugin,
        &subject,
        &key,
        VIKING_SEARCH_TOOL,
        &json!({"query":"status", "scope": root(&key)}),
    );
    assert!(!result.contains("error"));
    let request = requests
        .recv_timeout(StdDuration::from_secs(2))
        .expect("request");
    assert!(request.starts_with("POST /api/v1/search/find "));
    assert!(request.contains(&root(&key)));
    assert!(!request.contains("search-session"));
}

#[test]
fn export_is_typed_unsupported_without_snapshot_protocol() {
    let (subject, key) = subject("export-session");
    let plugin = plugin("http://127.0.0.1:9".to_string(), &key);
    let actor = actor(&subject);
    let mut sink = TestExportSink;
    assert!(matches!(
        MemoryProviderPlugin::export_subject(&plugin, &subject, &key, &actor, &mut sink),
        Err(MemoryProviderGovernanceError::Unsupported {
            operation: MemoryGovernanceOperation::Export,
            ..
        })
    ));
}

#[test]
fn forget_enumerates_subject_files_and_proves_tree_and_search_miss() {
    let (subject, key) = subject("forget-session");
    let subject_root = root(&key);
    let first = format!("{subject_root}/preferences/one.md");
    let second = format!("{subject_root}/preferences/two.md");
    let tree = format!(r#"{{"result":{{"children":[{{"uri":"{first}"}},{{"uri":"{second}"}}]}}}}"#);
    let (endpoint, requests) = mock_server(vec![
        (200, tree),
        (200, "{}".to_string()),
        (200, "{}".to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, "{\"result\":[]}".to_string()),
    ]);
    let plugin = plugin(endpoint, &key);
    let actor = actor(&subject);

    let receipt = MemoryProviderPlugin::forget_subject(&plugin, &subject, &key, &actor)
        .expect("subject forget");
    assert_eq!(receipt.deleted_records, 2);
    let captured = (0..6)
        .map(|_| {
            requests
                .recv_timeout(StdDuration::from_secs(2))
                .expect("request")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        captured
            .iter()
            .filter(|request| request.starts_with("DELETE /api/v1/fs?"))
            .count(),
        3
    );
    assert!(captured
        .iter()
        .any(|request| request.starts_with("POST /api/v1/search/find ")));
    assert!(captured
        .iter()
        .all(|request| request.contains(&key.as_str())));
}

#[test]
fn retention_keeps_newest_subject_record() {
    let (subject, key) = subject("retention-session");
    let subject_root = root(&key);
    let newest = format!("{subject_root}/preferences/new.md");
    let oldest = format!("{subject_root}/preferences/old.md");
    let tree =
        format!(r#"{{"result":{{"children":[{{"uri":"{newest}"}},{{"uri":"{oldest}"}}]}}}}"#);
    let (endpoint, requests) = mock_server(vec![
        (200, tree),
        (200, "{\"created_at\":200}".to_string()),
        (200, "{\"created_at\":100}".to_string()),
        (200, "{}".to_string()),
    ]);
    let plugin = plugin(endpoint, &key);
    let policy = MemoryRetentionPolicy::new(None, Some(1)).expect("policy");
    let actor = actor(&subject);

    let receipt = MemoryProviderPlugin::retention_sweep(&plugin, &subject, &key, &policy, &actor)
        .expect("retention sweep");
    assert_eq!(receipt.deleted_records, 1);
    let captured = (0..4)
        .map(|_| {
            requests
                .recv_timeout(StdDuration::from_secs(2))
                .expect("request")
        })
        .collect::<Vec<_>>();
    let delete = captured
        .iter()
        .find(|request| request.starts_with("DELETE /api/v1/fs?"))
        .expect("delete request");
    assert!(delete.contains("old.md"));
    assert!(!delete.contains("new.md"));
}

#[test]
fn content_write_uri_is_opaque_subject_scoped() {
    let (_, key) = subject("write-session");
    let plugin = plugin("http://127.0.0.1:9".to_string(), &key);
    let state = plugin.state_for(&key).expect("state");
    let body = content_write_body(&state, "preferences", "fact");
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with(&root(&key)));
    assert!(!uri.contains("write-session"));
}
