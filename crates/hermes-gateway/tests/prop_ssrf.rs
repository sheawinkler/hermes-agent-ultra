//! Property 11: SSRF protection blocks private IPs
//! **Validates: Requirement 22.2**
//!
//! For any private/reserved IPv4 address in a URL, is_safe_url returns false.
//! For any public IPv4 address in an HTTP/HTTPS URL, is_safe_url returns true.

use proptest::prelude::*;

use hermes_gateway::is_safe_url;

// ---------------------------------------------------------------------------
// Strategies for IP addresses
// ---------------------------------------------------------------------------

/// Generate a private IPv4 address from known private ranges.
fn arb_private_ipv4() -> impl Strategy<Value = String> {
    prop_oneof![
        // 10.0.0.0/8
        (0u8..=255, 0u8..=255, 0u8..=255)
            .prop_map(|(b, c, d)| format!("10.{b}.{c}.{d}")),
        // 172.16.0.0/12
        (16u8..=31, 0u8..=255, 0u8..=255)
            .prop_map(|(b, c, d)| format!("172.{b}.{c}.{d}")),
        // 192.168.0.0/16
        (0u8..=255, 0u8..=255)
            .prop_map(|(c, d)| format!("192.168.{c}.{d}")),
        // 127.0.0.0/8
        (0u8..=255, 0u8..=255, 0u8..=255)
            .prop_map(|(b, c, d)| format!("127.{b}.{c}.{d}")),
        // 169.254.0.0/16
        (0u8..=255, 0u8..=255)
            .prop_map(|(c, d)| format!("169.254.{c}.{d}")),
    ]
}

/// Generate a public IPv4 address that is NOT in any private/reserved range.
fn arb_public_ipv4() -> impl Strategy<Value = String> {
    // Use well-known public ranges: 1.x.x.x, 2.x.x.x, 8.x.x.x, etc.
    // Avoid: 0.x, 10.x, 100.64-127.x, 127.x, 169.254.x, 172.16-31.x, 192.168.x, 198.18-19.x
    prop_oneof![
        // 1.0.0.0 - 9.255.255.255
        (1u8..10, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 11.0.0.0 - 99.255.255.255
        (11u8..100, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 128.0.0.0 - 168.255.255.255
        (128u8..169, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 170.0.0.0 - 171.255.255.255
        (170u8..172, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 173.0.0.0 - 191.255.255.255
        (173u8..192, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 193.0.0.0 - 197.255.255.255
        (193u8..198, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        // 200.0.0.0 - 223.255.255.255
        (200u8..224, 0u8..=255, 0u8..=255, 1u8..=254)
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
    ]
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_private_ip_blocked(ip in arb_private_ipv4()) {
        let url = format!("http://{ip}/api");
        prop_assert!(
            !is_safe_url(&url),
            "Private IP {} should be blocked but is_safe_url returned true",
            ip
        );
    }

    #[test]
    fn prop_public_ip_allowed(ip in arb_public_ipv4()) {
        let url = format!("https://{ip}/api");
        prop_assert!(
            is_safe_url(&url),
            "Public IP {} should be allowed but is_safe_url returned false",
            ip
        );
    }
}
