//! Best-effort public IP geolocation for device activation reporting.
//!
//! Provider order favors services reachable from mainland China, then international
//! fallbacks (Electron parity). Lookup is optional — activation proceeds without geo.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Resolved geo fields for activation (`country` / `province` / `city` / `region` / `operator`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeoIpInfo {
    pub public_ip: String,
    pub country: String,
    pub country_code: String,
    pub province: String,
    pub city: String,
    pub region: String,
    pub operator: String,
    pub postal: String,
    pub latitude: String,
    pub longitude: String,
    pub timezone: String,
    pub currency: String,
}

const SKIP_ENV: &str = "HERMES_SKIP_GEOIP";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve geo via a short provider chain. Returns `None` when disabled or all providers fail.
pub async fn resolve_geo_ip() -> Option<GeoIpInfo> {
    if geoip_disabled() {
        debug!("geoip lookup skipped ({SKIP_ENV})");
        return None;
    }

    let client = match build_client() {
        Ok(c) => c,
        Err(err) => {
            warn!(error = %err, "geoip client init failed");
            return None;
        }
    };

    for provider in PROVIDERS {
        match provider.fetch(&client).await {
            Some(info) if info.has_location() => {
                debug!(provider = provider.name, ip = %info.public_ip, "geoip resolved");
                return Some(info);
            }
            Some(_) => debug!(provider = provider.name, "geoip response missing location fields"),
            None => debug!(provider = provider.name, "geoip provider unavailable"),
        }
    }

    warn!("all geoip providers failed; activation will omit geo fields");
    None
}

fn geoip_disabled() -> bool {
    std::env::var(SKIP_ENV)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn build_client() -> Result<Client, reqwest::Error> {
    Client::builder().timeout(LOOKUP_TIMEOUT).build()
}

impl GeoIpInfo {
    pub fn has_location(&self) -> bool {
        !self.country.is_empty()
            || !self.province.is_empty()
            || !self.city.is_empty()
            || !self.public_ip.is_empty()
    }
}

struct GeoIpProviderEntry {
    name: &'static str,
    url: &'static str,
    parse: fn(&str) -> Option<GeoIpInfo>,
}

impl GeoIpProviderEntry {
    async fn fetch(&self, client: &Client) -> Option<GeoIpInfo> {
        let resp = client.get(self.url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body = resp.text().await.ok()?;
        (self.parse)(&body)
    }
}

/// Domestic-first, then international fallbacks.
static PROVIDERS: [&GeoIpProviderEntry; 4] = [
    &GeoIpProviderEntry {
        name: "ip.sb",
        url: "https://api.ip.sb/geoip",
        parse: parse_ip_sb,
    },
    &GeoIpProviderEntry {
        name: "speedtest.cn",
        url: "https://forge.speedtest.cn/api/location/info",
        parse: parse_speedtest_cn,
    },
    &GeoIpProviderEntry {
        name: "ipapi.co",
        url: "https://ipapi.co/json/",
        parse: parse_ipapi_co,
    },
    &GeoIpProviderEntry {
        name: "ip-api.com",
        url: "http://ip-api.com/json/?fields=status,message,country,countryCode,regionName,city,isp,query,timezone,lat,lon,zip",
        parse: parse_ip_api_com,
    },
];

fn parse_ip_sb(body: &str) -> Option<GeoIpInfo> {
    #[derive(Deserialize)]
    struct Raw {
        ip: Option<String>,
        country: Option<String>,
        country_code: Option<String>,
        region: Option<String>,
        city: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        isp: Option<String>,
        organization: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    let operator = raw
        .isp
        .or(raw.organization)
        .unwrap_or_default()
        .trim()
        .to_string();
    let city = raw.city.unwrap_or_default().trim().to_string();
    Some(GeoIpInfo {
        public_ip: raw.ip.unwrap_or_default().trim().to_string(),
        country: raw.country.unwrap_or_default().trim().to_string(),
        country_code: raw.country_code.unwrap_or_default().trim().to_string(),
        province: raw.region.unwrap_or_default().trim().to_string(),
        city: city.clone(),
        region: city,
        operator,
        latitude: raw
            .latitude
            .map(|v| v.to_string())
            .unwrap_or_default(),
        longitude: raw
            .longitude
            .map(|v| v.to_string())
            .unwrap_or_default(),
        ..GeoIpInfo::default()
    })
}

fn parse_speedtest_cn(body: &str) -> Option<GeoIpInfo> {
    #[derive(Deserialize)]
    struct Raw {
        ip: Option<String>,
        country: Option<String>,
        province: Option<String>,
        city: Option<String>,
        #[serde(alias = "isp")]
        operator: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    let province = raw.province.unwrap_or_default().trim().to_string();
    let city = raw.city.unwrap_or_default().trim().to_string();
    Some(GeoIpInfo {
        public_ip: raw.ip.unwrap_or_default().trim().to_string(),
        country: raw.country.unwrap_or_default().trim().to_string(),
        province: province.clone(),
        city: city.clone(),
        region: if city.is_empty() { province } else { city },
        operator: raw.operator.unwrap_or_default().trim().to_string(),
        ..GeoIpInfo::default()
    })
}

fn parse_ipapi_co(body: &str) -> Option<GeoIpInfo> {
    #[derive(Deserialize)]
    struct Raw {
        ip: Option<String>,
        country_name: Option<String>,
        country_code: Option<String>,
        region: Option<String>,
        city: Option<String>,
        postal: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        timezone: Option<String>,
        currency: Option<String>,
        org: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    let city = raw.city.unwrap_or_default().trim().to_string();
    Some(GeoIpInfo {
        public_ip: raw.ip.unwrap_or_default().trim().to_string(),
        country: raw.country_name.unwrap_or_default().trim().to_string(),
        country_code: raw.country_code.unwrap_or_default().trim().to_string(),
        province: raw.region.unwrap_or_default().trim().to_string(),
        city: city.clone(),
        region: city,
        operator: raw.org.unwrap_or_default().trim().to_string(),
        postal: raw.postal.unwrap_or_default().trim().to_string(),
        latitude: raw
            .latitude
            .map(|v| v.to_string())
            .unwrap_or_default(),
        longitude: raw
            .longitude
            .map(|v| v.to_string())
            .unwrap_or_default(),
        timezone: raw.timezone.unwrap_or_default().trim().to_string(),
        currency: raw.currency.unwrap_or_default().trim().to_string(),
    })
}

fn parse_ip_api_com(body: &str) -> Option<GeoIpInfo> {
    #[derive(Deserialize)]
    struct Raw {
        status: Option<String>,
        country: Option<String>,
        country_code: Option<String>,
        region_name: Option<String>,
        city: Option<String>,
        isp: Option<String>,
        query: Option<String>,
        timezone: Option<String>,
        lat: Option<f64>,
        lon: Option<f64>,
        zip: Option<String>,
    }

    let raw: Raw = serde_json::from_str(body).ok()?;
    if raw.status.as_deref() != Some("success") {
        return None;
    }
    let city = raw.city.unwrap_or_default().trim().to_string();
    Some(GeoIpInfo {
        public_ip: raw.query.unwrap_or_default().trim().to_string(),
        country: raw.country.unwrap_or_default().trim().to_string(),
        country_code: raw.country_code.unwrap_or_default().trim().to_string(),
        province: raw.region_name.unwrap_or_default().trim().to_string(),
        city: city.clone(),
        region: city,
        operator: raw.isp.unwrap_or_default().trim().to_string(),
        postal: raw.zip.unwrap_or_default().trim().to_string(),
        latitude: raw.lat.map(|v| v.to_string()).unwrap_or_default(),
        longitude: raw.lon.map(|v| v.to_string()).unwrap_or_default(),
        timezone: raw.timezone.unwrap_or_default().trim().to_string(),
        ..GeoIpInfo::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ip_sb_sample() {
        let body = r#"{
            "country":"China",
            "country_code":"CN",
            "region":"Beijing",
            "city":"Beijing",
            "latitude":39.9042,
            "longitude":116.4074,
            "isp":"China Mobile",
            "ip":"203.0.113.1"
        }"#;
        let info = parse_ip_sb(body).expect("parse");
        assert_eq!(info.country, "China");
        assert_eq!(info.province, "Beijing");
        assert_eq!(info.operator, "China Mobile");
        assert_eq!(info.public_ip, "203.0.113.1");
    }

    #[test]
    fn parse_ipapi_co_sample() {
        let body = r#"{
            "ip":"203.0.113.2",
            "country_name":"China",
            "country_code":"CN",
            "region":"Shanghai",
            "city":"Shanghai",
            "org":"China Telecom"
        }"#;
        let info = parse_ipapi_co(body).expect("parse");
        assert_eq!(info.country, "China");
        assert_eq!(info.province, "Shanghai");
        assert_eq!(info.operator, "China Telecom");
    }

    #[test]
    fn parse_ip_api_com_requires_success_status() {
        let fail = r#"{"status":"fail","message":"private range"}"#;
        assert!(parse_ip_api_com(fail).is_none());

        let ok = r#"{
            "status":"success",
            "country":"China",
            "countryCode":"CN",
            "regionName":"Guangdong",
            "city":"Shenzhen",
            "isp":"China Unicom",
            "query":"203.0.113.3"
        }"#;
        let info = parse_ip_api_com(ok).expect("parse");
        assert_eq!(info.city, "Shenzhen");
        assert_eq!(info.operator, "China Unicom");
    }

    #[test]
    fn has_location_requires_meaningful_fields() {
        assert!(!GeoIpInfo::default().has_location());
        assert!(
            GeoIpInfo {
                public_ip: "1.2.3.4".into(),
                ..Default::default()
            }
            .has_location()
        );
    }
}
