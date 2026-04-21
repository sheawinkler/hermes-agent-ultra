use std::path::PathBuf;

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &str) -> Value {
    let full = repo_root().join(path);
    let raw = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed reading {}: {}", full.display(), e));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed parsing {}: {}", full.display(), e))
}

#[test]
fn test_intent_mapping_ratio_meets_gate() {
    let payload = read_json("docs/parity/test-intent-mapping.json");
    let ratio = payload["summary"]["mapping_ratio"]
        .as_f64()
        .expect("mapping_ratio should be number");
    assert!(
        ratio >= 0.9,
        "test-intent mapping ratio below gate: {}",
        ratio
    );
}

#[test]
fn test_adapter_matrix_has_no_placeholder_status() {
    let payload = read_json("docs/parity/adapter-feature-matrix.json");
    let non_native = payload["summary"]["non_rust_native"]
        .as_i64()
        .expect("non_rust_native should be integer");
    let placeholders = payload["summary"]["placeholder_status_entries"]
        .as_i64()
        .expect("placeholder_status_entries should be integer");
    assert_eq!(
        non_native, 0,
        "adapter matrix has non-rust-native entries: {}",
        non_native
    );
    assert_eq!(
        placeholders, 0,
        "adapter matrix has placeholder entries: {}",
        placeholders
    );
}

#[test]
fn test_divergence_registry_has_ownership_and_review_fields() {
    let payload = read_json("docs/parity/intentional-divergence.json");
    let items = payload["items"].as_array().expect("items should be array");
    assert!(!items.is_empty(), "expected intentional divergence entries");
    for item in items {
        for field in [
            "id",
            "status",
            "owner",
            "ticket",
            "last_reviewed",
            "review_date",
            "rationale",
        ] {
            assert!(
                item.get(field).is_some(),
                "divergence item missing required field: {}",
                field
            );
        }
        let owner = item["owner"].as_str().unwrap_or_default();
        assert!(
            !owner.trim().is_empty(),
            "divergence item owner must be non-empty"
        );
    }
}

#[test]
fn test_shared_diff_classification_covers_matrix_items() {
    let matrix = read_json("docs/parity/parity-matrix.json");
    let classification = read_json("docs/parity/shared-different-classification.json");

    let matrix_paths = matrix["top_shared_different"]
        .as_array()
        .expect("top_shared_different should be array");
    let class_items = classification["items"]
        .as_array()
        .expect("classification items should be array");
    let class_paths: std::collections::BTreeSet<String> = class_items
        .iter()
        .filter_map(|item| item["path"].as_str().map(|s| s.to_string()))
        .collect();

    for row in matrix_paths {
        let p = row["path"].as_str().expect("path should be string");
        assert!(
            class_paths.contains(p),
            "shared-different path is unclassified: {}",
            p
        );
    }
}
