//! Fixed development recipe. The plugin cannot provide a URL, headers, body shape or proxy.
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use std::{io::Read, net::IpAddr, sync::Arc, time::Duration};
use stream_dock_manager_plugin_host::permission_rpc::TEXT_LIMIT;
use zeroize::Zeroizing;

const HOST: &str = "api.example.test";
const ENDPOINT: &str = "https://api.example.test/v1/summary";

// Restrict connections to ordinary global unicast. Also exclude special-purpose
// protocol assignments, documentation and transition address blocks.
fn public_destination(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192
                    && (b == 168
                        || (b == 0 && matches!(c, 0 | 2))
                        || (b == 88 && c == 99)
                        || (b == 31 && c == 196)
                        || (b == 52 && c == 193)
                        || (b == 175 && c == 48)))
                && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            (0x2000..0x4000).contains(&a)
                && !(a == 0x2001 && (b < 0x200 || b == 0xdb8))
                && !matches!(a, 0x2002 | 0x3ffe | 0x3fff)
                && !(a == 0x2620 && b == 0x4f)
        }
    }
}
struct RecipeResolver(&'static str);
impl Resolve for RecipeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = self.0;
        Box::pin(async move {
            if name.as_str() != host {
                return Err("destination unavailable".into());
            }
            let addresses: Vec<_> = tokio::net::lookup_host((host, 443))
                .await?
                .take(65)
                .collect();
            if addresses.is_empty()
                || addresses.len() > 64
                || addresses.iter().any(|addr| !public_destination(addr.ip()))
            {
                return Err("destination unavailable".into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    summary: String,
}

pub fn summarize(
    reviewed: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Zeroizing<String>, &'static str> {
    summary(reviewed, None, cancelled)
}
pub fn summary(
    reviewed: &str,
    authorization: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Zeroizing<String>, &'static str> {
    if reviewed.len() > TEXT_LIMIT {
        return Err("too_large");
    }
    let mut request = client(HOST)?
        .post(ENDPOINT)
        .json(&serde_json::json!({"text":reviewed}));
    if let Some(key) = authorization {
        use reqwest::header::{HeaderValue, AUTHORIZATION};
        let header = Zeroizing::new(format!("Bearer {key}"));
        let mut value = HeaderValue::from_str(&header).map_err(|_| "operation_unavailable")?;
        value.set_sensitive(true);
        request = request.header(AUTHORIZATION, value);
    }
    let bytes = send(request, cancelled)?;
    let result: Answer = serde_json::from_slice(&bytes).map_err(|_| "operation_unavailable")?;
    let answer = Zeroizing::new(result.summary);
    if authorization.is_some_and(|key| answer.contains(key)) {
        return Err("operation_unavailable");
    }
    Ok(answer)
}
/// The local example accepts one synthetic payload. Its ephemeral certificate is
/// supplied by the authenticated plugin and trusted for this single connection.
pub fn webhook(
    endpoint: &stream_dock_manager_plugin_host::permission_rpc::WebhookEndpoint,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), &'static str> {
    use stream_dock_manager_plugin_host::{
        permission_rpc::WebhookEndpoint, permissions::loopback_http_address,
    };
    let mut client = reqwest::blocking::Client::builder();
    match endpoint {
        WebhookEndpoint::Https { certificate } => {
            let cert = reqwest::Certificate::from_pem(certificate.as_bytes())
                .map_err(|_| "invalid_request")?;
            client = client.tls_certs_only([cert]).https_only(true);
        }
        WebhookEndpoint::Http { origin } => {
            let addr = loopback_http_address(origin).ok_or("invalid_request")?;
            client = client.resolve("localhost", addr);
        }
    }
    let client = client
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|_| "operation_unavailable")?;
    if cancelled() {
        return Err("cancelled");
    }
    let response = client
        .post(format!("{}/webhooks/manager", endpoint.origin()))
        .json(&serde_json::json!({"event":"ping"}))
        .send()
        .map_err(|_| "operation_unavailable")?;
    if cancelled() {
        return Err("cancelled");
    }
    if response.status() != reqwest::StatusCode::NO_CONTENT {
        return Err("operation_unavailable");
    }
    Ok(())
}
fn client(host: &'static str) -> Result<reqwest::blocking::Client, &'static str> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .dns_resolver(Arc::new(RecipeResolver(host)))
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "operation_unavailable")
}
fn send(
    request: reqwest::blocking::RequestBuilder,
    cancelled: &dyn Fn() -> bool,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let request = request
        .header("Accept", "application/json")
        .header("Accept-Encoding", "identity");
    if cancelled() {
        return Err("cancelled");
    }
    let response = request.send().map_err(|_| "operation_unavailable")?;
    if !response.status().is_success()
        || response.headers().contains_key("content-encoding")
        || response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/json")
    {
        return Err("operation_unavailable");
    }
    let mut bytes = Zeroizing::new(Vec::new());
    response
        .take((TEXT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "operation_unavailable")?;
    if cancelled() {
        return Err("cancelled");
    }
    if bytes.len() > TEXT_LIMIT {
        return Err("too_large");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recipe_limits_actual_connection_destinations() {
        for value in [
            "127.0.0.1",
            "0.5.6.7",
            "192.168.9.1",
            "100.64.0.1",
            "169.254.12.1",
            "198.19.0.1",
            "224.0.0.1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "2002::1",
            "2001:db8::1",
            "64:ff9b::1",
            "3fff::1",
        ] {
            assert!(!public_destination(value.parse().unwrap()), "{value}");
        }
        assert!(public_destination("93.184.216.34".parse().unwrap()));
        assert!(public_destination("2606:4700::1111".parse().unwrap()));
    }
}
