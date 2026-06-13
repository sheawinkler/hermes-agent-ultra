//! Bounded invariant coverage: SSRF protection blocks private IPs
//! **Validates: Requirement 22.2**
//!
//! Private/reserved IPv4 addresses in URLs must be blocked, while selected
//! public IPv4 HTTP/HTTPS URLs must be allowed.

use hermes_gateway::is_safe_url;

fn private_ipv4_cases() -> &'static [&'static str] {
    &[
        "10.0.0.1",
        "10.255.255.254",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.0.1",
        "192.168.255.254",
        "127.0.0.1",
        "127.255.255.254",
        "169.254.0.1",
        "169.254.255.254",
    ]
}

fn public_ipv4_cases() -> &'static [&'static str] {
    &[
        "1.1.1.1",
        "8.8.8.8",
        "11.0.0.1",
        "99.255.255.254",
        "128.0.0.1",
        "168.255.255.254",
        "170.0.0.1",
        "171.255.255.254",
        "173.0.0.1",
        "191.255.255.254",
        "193.0.0.1",
        "197.255.255.254",
        "200.0.0.1",
        "223.255.255.254",
    ]
}

#[test]
fn private_ip_blocked() {
    for ip in private_ipv4_cases() {
        let url = format!("http://{ip}/api");
        assert!(
            !is_safe_url(&url),
            "Private IP {} should be blocked but is_safe_url returned true",
            ip
        );
    }
}

#[test]
fn public_ip_allowed() {
    for ip in public_ipv4_cases() {
        let url = format!("https://{ip}/api");
        assert!(
            is_safe_url(&url),
            "Public IP {} should be allowed but is_safe_url returned false",
            ip
        );
    }
}
