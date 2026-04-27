use assert_cmd::Command;
use flate2::read::GzDecoder;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::Path;

fn write_replay_fixture(home: &Path) {
    let replay_dir = home.join("logs").join("replay");
    fs::create_dir_all(&replay_dir).expect("create replay dir");

    let good = r#"{"seq":1,"event":"user","prev_hash":"seed","event_hash":"h1","payload":{"text":"hi"}}
{"seq":2,"event":"assistant","prev_hash":"h1","event_hash":"h2","payload":{"text":"hello"}}
"#;
    let bad = r#"{"seq":1,"event":"user","prev_hash":"seed","event_hash":"x1","payload":{"text":"hi"}}
{"seq":2,"event":"assistant","prev_hash":"BROKEN","event_hash":"x2","payload":{"text":"hello"}}
"#;
    fs::write(replay_dir.join("session-good.jsonl"), good).expect("write good replay");
    fs::write(replay_dir.join("session-bad.jsonl"), bad).expect("write bad replay");
}

fn run_cmd(home: &Path, args: &[&str]) -> String {
    let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
    cmd.env("HERMES_HOME", home).args(args);
    let out = cmd.assert().success().get_output().stdout.clone();
    String::from_utf8(out).expect("utf8")
}

fn extract_bundle_member(bundle_path: &Path, member: &str) -> Vec<u8> {
    let bytes = fs::read(bundle_path).expect("read bundle");
    let decoder = GzDecoder::new(bytes.as_slice());
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().expect("tar entries");
    while let Some(Ok(mut entry)) = entries.next() {
        let path = entry.path().expect("entry path");
        if path.to_string_lossy() == member {
            let mut body = Vec::new();
            entry.read_to_end(&mut body).expect("read entry");
            return body;
        }
    }
    panic!("member not found in bundle: {member}");
}

#[test]
fn e2e_replay_suite_provider_auth_gateway_and_deterministic_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    write_replay_fixture(home);

    let snapshot = home.join("snapshot.json");
    fs::write(
        &snapshot,
        r#"{"generated_at":"1970-01-01T00:00:00Z","mode":"deterministic-test"}"#,
    )
    .expect("write snapshot");

    // Provider/auth/gateway command flow smoke checks.
    let model_out = run_cmd(home, &["model"]);
    assert!(
        model_out.contains("Current model"),
        "model command output missing current model line: {model_out:?}"
    );
    let auth_status_out = run_cmd(home, &["auth", "status", "nous"]);
    assert!(
        auth_status_out.contains("Auth status"),
        "auth status output missing summary line: {auth_status_out:?}"
    );
    let gateway_status_out = run_cmd(home, &["gateway", "status"]);
    assert!(
        gateway_status_out.contains("Gateway status"),
        "gateway status output missing banner: {gateway_status_out:?}"
    );

    // Deterministic incident-pack run #1.
    let bundle_a = home.join("incident-a.tar.gz");
    let out_a = run_cmd(
        home,
        &[
            "incident-pack",
            "--snapshot",
            snapshot.to_str().expect("snapshot path utf8"),
            "--output",
            bundle_a.to_str().expect("bundle path utf8"),
            "--json",
        ],
    );
    let payload_a: Value = serde_json::from_str(&out_a).expect("parse incident-pack json");
    assert_eq!(payload_a["ok"], true);
    assert_eq!(payload_a["deterministic"], true);
    assert!(bundle_a.exists(), "bundle A should exist");

    // Deterministic incident-pack run #2.
    let bundle_b = home.join("incident-b.tar.gz");
    let out_b = run_cmd(
        home,
        &[
            "incident-pack",
            "--snapshot",
            snapshot.to_str().expect("snapshot path utf8"),
            "--output",
            bundle_b.to_str().expect("bundle path utf8"),
            "--json",
        ],
    );
    let payload_b: Value = serde_json::from_str(&out_b).expect("parse incident-pack json");
    assert_eq!(payload_b["ok"], true);
    assert!(bundle_b.exists(), "bundle B should exist");

    let manifest_a = extract_bundle_member(&bundle_a, "doctor/replay/manifest.json");
    let manifest_b = extract_bundle_member(&bundle_b, "doctor/replay/manifest.json");
    assert_eq!(manifest_a, manifest_b, "manifest should be deterministic");

    let manifest: Value = serde_json::from_slice(&manifest_a).expect("manifest json");
    assert_eq!(manifest["generated_at"], "1970-01-01T00:00:00Z");
    assert_eq!(manifest["totals"]["files"], 2);
    assert_eq!(manifest["totals"]["events"], 4);
    assert_eq!(manifest["totals"]["hash_chain_ok"], false);
    assert!(
        manifest["files"]
            .as_array()
            .expect("files array")
            .iter()
            .any(|f| f["file"] == "session-bad.jsonl" && f["hash_chain_ok"] == false),
        "expected broken replay chain to be reported"
    );
}
