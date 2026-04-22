//! SSRF (Server-Side Request Forgery) protection (Requirement 22.2).
//!
//! Validates outbound URLs to prevent access to internal/private networks
//! and cloud metadata endpoints.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::Mutex;

use url::Url;

use hermes_core::errors::GatewayError;

const METADATA_HOSTNAMES: [&str; 3] = [
    "metadata.google.internal",
    "metadata.goog",
    "metadata.internal",
];

static ALLOW_PRIVATE_URLS_CACHE: Mutex<Option<bool>> = Mutex::new(None);

fn parse_bool_like(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
fn config_allow_private_urls() -> bool {
    false
}

#[cfg(not(test))]
fn config_allow_private_urls() -> bool {
    match hermes_config::load_config(None) {
        Ok(cfg) => {
            if cfg.security.allow_private_urls {
                return true;
            }
            cfg.tools_config
                .per_tool
                .get("browser")
                .and_then(|v| v.get("allow_private_urls"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

fn global_allow_private_urls() -> bool {
    let mut guard = ALLOW_PRIVATE_URLS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(v) = *guard {
        return v;
    }

    let resolved = std::env::var("HERMES_ALLOW_PRIVATE_URLS")
        .ok()
        .and_then(|v| parse_bool_like(&v))
        .unwrap_or_else(config_allow_private_urls);
    *guard = Some(resolved);
    resolved
}

#[cfg(test)]
fn reset_allow_private_cache_for_tests() {
    let mut guard = ALLOW_PRIVATE_URLS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

// ---------------------------------------------------------------------------
// Private IP range checks
// ---------------------------------------------------------------------------

/// Check if an IPv4 address falls within a private/reserved range.
///
/// Blocks:
/// - 10.0.0.0/8 (Class A private)
/// - 172.16.0.0/12 (Class B private)
/// - 192.168.0.0/16 (Class C private)
/// - 127.0.0.0/8 (Loopback)
/// - 169.254.0.0/16 (Link-local, includes cloud metadata)
fn is_private_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();

    // 10.0.0.0/8
    if octets[0] == 10 {
        return true;
    }

    // 172.16.0.0/12 (172.16.0.0 - 172.31.255.255)
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return true;
    }

    // 192.168.0.0/16
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }

    // 127.0.0.0/8 (Loopback)
    if octets[0] == 127 {
        return true;
    }

    // 169.254.0.0/16 (Link-local, includes cloud metadata)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }

    // 0.0.0.0/8 (Current network)
    if octets[0] == 0 {
        return true;
    }

    // 100.64.0.0/10 (Carrier-grade NAT)
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return true;
    }

    // 198.18.0.0/15 (Benchmarking)
    if octets[0] == 198 && (18..=19).contains(&octets[1]) {
        return true;
    }

    false
}

/// Check if an IPv6 address falls within a private/reserved range.
///
/// Blocks:
/// - ::1/128 (Loopback)
/// - fe80::/10 (Link-local)
/// - fc00::/7 (Unique local / ULA)
/// - ::ffff:x.x.x.x (IPv4-mapped, delegates to IPv4 check)
fn is_private_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();

    // ::1 (Loopback)
    if ip.is_loopback() {
        return true;
    }

    // fe80::/10 (Link-local)
    // First 10 bits: 1111 1110 10xx xxxx
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }

    // fc00::/7 (Unique local addresses: fc00::/7 and fd00::/7)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }

    // ::ffff:x.x.x.x (IPv4-mapped IPv6)
    let segments = ip.segments();
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        let ipv4 = ip.to_ipv4_mapped().expect("valid ipv4-mapped address");
        return is_private_ipv4(&ipv4);
    }

    false
}

/// Check whether an IP address is in a private/reserved range.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(v4),
        IpAddr::V6(v6) => is_private_ipv6(v6),
    }
}

// ---------------------------------------------------------------------------
// Cloud metadata endpoint check
// ---------------------------------------------------------------------------

fn is_always_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // Entire link-local range (includes AWS/GCP/Azure metadata)
            (octets[0] == 169 && octets[1] == 254)
                // Alibaba cloud metadata endpoint
                || octets == [100, 100, 100, 200]
        }
        // AWS metadata over IPv6
        IpAddr::V6(v6) => *v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
    }
}

/// Check if an IP address matches known cloud metadata endpoints.
#[cfg(test)]
fn is_cloud_metadata(ip: &IpAddr) -> bool {
    is_always_blocked_ip(ip)
}

fn is_metadata_hostname(host: &str) -> bool {
    METADATA_HOSTNAMES.contains(&host)
}

fn resolve_host_ips(host: &str, port: u16) -> Result<Vec<IpAddr>, GatewayError> {
    let addrs = (host, port).to_socket_addrs().map_err(|e| {
        GatewayError::ConnectionFailed(format!("Failed to resolve host '{}': {}", host, e))
    })?;
    let ips: Vec<IpAddr> = addrs.map(|addr| addr.ip()).collect();
    if ips.is_empty() {
        return Err(GatewayError::ConnectionFailed(format!(
            "Host '{}' resolved to no addresses",
            host
        )));
    }
    Ok(ips)
}

fn validate_ip_policy(
    host: &str,
    ip: &IpAddr,
    allow_private_urls: bool,
) -> Result<(), GatewayError> {
    if is_always_blocked_ip(ip) {
        return Err(GatewayError::ConnectionFailed(format!(
            "URL resolves to cloud metadata endpoint: {} -> {}",
            host, ip
        )));
    }
    if !allow_private_urls && is_private_ip(ip) {
        return Err(GatewayError::ConnectionFailed(format!(
            "URL resolves to private/internal IP address: {} -> {}",
            host, ip
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether a URL is safe to access (not a private/internal address).
///
/// This function:
/// 1. Parses the URL
/// 2. Resolves the hostname to IP addresses
/// 3. Rejects private/reserved ranges unless explicitly allowed
/// 4. Always rejects cloud metadata endpoints
///
/// Returns `true` if the URL is safe, `false` otherwise.
pub fn is_safe_url(url: &str) -> bool {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };

    match parsed.scheme() {
        "http" | "https" => {}
        _ => return false,
    };

    let host = match parsed.host_str() {
        Some(h) => h,
        None => return false,
    };
    let lowercase_host = host.to_ascii_lowercase();
    if is_metadata_hostname(&lowercase_host) {
        tracing::warn!("Blocked request to metadata hostname: {}", host);
        return false;
    }

    let allow_private_urls = global_allow_private_urls();

    if let Ok(ip) = IpAddr::from_str(host) {
        return validate_ip_policy(host, &ip, allow_private_urls).is_ok();
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let ips = match resolve_host_ips(host, port) {
        Ok(ips) => ips,
        Err(err) => {
            tracing::warn!("Blocked unresolved URL host '{}': {}", host, err);
            return false;
        }
    };

    for ip in &ips {
        if let Err(err) = validate_ip_policy(host, ip, allow_private_urls) {
            tracing::warn!("{}", err);
            return false;
        }
    }

    true
}

/// Validate a URL and return it if it passes SSRF protection checks.
///
/// Returns the parsed `Url` if safe, or a `GatewayError` if the URL
/// is potentially dangerous.
pub fn validate_url(url: &str) -> Result<Url, GatewayError> {
    let parsed = Url::parse(url)
        .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid URL '{}': {}", url, e)))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(GatewayError::ConnectionFailed(format!(
                "Unsupported URL scheme '{}': only http and https are allowed",
                other
            )));
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| GatewayError::ConnectionFailed(format!("URL has no host: {}", url)))?;
    let lowercase_host = host.to_ascii_lowercase();

    if is_metadata_hostname(&lowercase_host) {
        return Err(GatewayError::ConnectionFailed(format!(
            "URL hostname is blocked: {}",
            host
        )));
    }

    let allow_private_urls = global_allow_private_urls();

    if let Ok(ip) = IpAddr::from_str(host) {
        validate_ip_policy(host, &ip, allow_private_urls)?;
        return Ok(parsed);
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let ips = resolve_host_ips(host, port)?;
    for ip in &ips {
        validate_ip_policy(host, ip, allow_private_urls)?;
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                old: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
            reset_allow_private_cache_for_tests();
        }
    }

    #[test]
    fn test_private_ipv4_ranges() {
        assert!(is_private_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(10, 255, 255, 255)));
        assert!(is_private_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(172, 31, 255, 255)));
        assert!(is_private_ipv4(&Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_private_ipv4(&Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_private_ipv4(&Ipv4Addr::new(0, 0, 0, 0)));

        assert!(!is_private_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(172, 15, 0, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_private_ipv4(&Ipv4Addr::new(192, 169, 0, 1)));
    }

    #[test]
    fn test_private_ipv6_ranges() {
        assert!(is_private_ipv6(&Ipv6Addr::LOCALHOST));
        assert!(is_private_ipv6(&"fe80::1".parse().expect("parse fe80")));
        assert!(is_private_ipv6(&"fc00::1".parse().expect("parse fc00")));
        assert!(is_private_ipv6(&"fd00::1".parse().expect("parse fd00")));

        assert!(is_private_ipv6(
            &"::ffff:127.0.0.1".parse().expect("parse mapped loopback")
        ));
        assert!(is_private_ipv6(
            &"::ffff:10.0.0.1".parse().expect("parse mapped private")
        ));

        assert!(!is_private_ipv6(
            &"2001:db8::1".parse().expect("parse docs")
        ));
        assert!(!is_private_ipv6(
            &"2606:4700:4700::1111".parse().expect("parse cloudflare")
        ));
    }

    #[test]
    fn test_cloud_metadata() {
        let ip: IpAddr = "169.254.169.254".parse().expect("parse metadata v4");
        assert!(is_cloud_metadata(&ip));

        let ip: IpAddr = "fd00:ec2::254".parse().expect("parse metadata v6");
        assert!(is_cloud_metadata(&ip));

        let ip: IpAddr = "8.8.8.8".parse().expect("parse public");
        assert!(!is_cloud_metadata(&ip));
    }

    #[test]
    fn test_is_safe_url_public_and_invalid() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _guard = EnvVarGuard::capture("HERMES_ALLOW_PRIVATE_URLS");
        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "false");
        reset_allow_private_cache_for_tests();

        assert!(is_safe_url("http://8.8.8.8/api"));
        assert!(!is_safe_url("not-a-url"));
        assert!(!is_safe_url("ftp://example.com"));
    }

    #[test]
    fn test_validate_url_private_ip_toggle() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _guard = EnvVarGuard::capture("HERMES_ALLOW_PRIVATE_URLS");

        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "false");
        reset_allow_private_cache_for_tests();
        assert!(validate_url("http://192.168.1.1/api").is_err());
        assert!(validate_url("http://localhost/api").is_err());

        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true");
        reset_allow_private_cache_for_tests();
        assert!(validate_url("http://192.168.1.1/api").is_ok());
        assert!(validate_url("http://localhost/api").is_ok());
        assert!(validate_url("http://100.100.100.100/api").is_ok());
        assert!(validate_url("http://198.18.0.1/api").is_ok());
    }

    #[test]
    fn test_validate_url_metadata_always_blocked() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _guard = EnvVarGuard::capture("HERMES_ALLOW_PRIVATE_URLS");
        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true");
        reset_allow_private_cache_for_tests();

        assert!(validate_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_url("http://169.254.170.2/v2/credentials").is_err());
        assert!(validate_url("http://169.254.169.253/").is_err());
        assert!(validate_url("http://169.254.42.99/anything").is_err());
        assert!(validate_url("http://100.100.100.200/latest/meta-data").is_err());
        assert!(validate_url("http://[fd00:ec2::254]/latest").is_err());
        assert!(validate_url("http://metadata.google.internal/computeMetadata/v1").is_err());
        assert!(validate_url("http://metadata.goog/computeMetadata/v1").is_err());
    }

    #[test]
    fn test_validate_url_dns_failure_is_blocked() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _guard = EnvVarGuard::capture("HERMES_ALLOW_PRIVATE_URLS");
        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true");
        reset_allow_private_cache_for_tests();

        assert!(validate_url("https://definitely-nonexistent.invalid").is_err());
        assert!(!is_safe_url("https://definitely-nonexistent.invalid"));
    }
}
