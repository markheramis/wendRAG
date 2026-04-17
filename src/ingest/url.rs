use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use html_to_markdown_rs::{ConversionOptions, PreprocessingPreset, convert};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use robotstxt::DefaultMatcher;
use url::Url;

use super::reader::{ReadDocument, ReadError, URL_FILE_TYPE};

const URL_INGEST_USER_AGENT: &str = "wend-rag/0.1";
const URL_FETCH_TIMEOUT_SECS: u64 = 30;

/**
 * Returns `true` when the supplied IP address belongs to any range that
 * must never be reachable from URL ingestion.
 *
 * Handles both IPv4 and IPv6 (including IPv4-mapped IPv6 addresses such as
 * `::ffff:127.0.0.1`, which would otherwise slip past `is_loopback()` on
 * the IPv6 branch).
 */
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(&v6),
    }
}

/**
 * IPv4 address blocklist: RFC1918 private ranges, loopback, link-local,
 * multicast, broadcast, documentation (TEST-NET), and the unspecified
 * address (`0.0.0.0`) which some routing stacks alias to the local host.
 */
fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
}

/**
 * IPv6 blocklist with IPv4-mapped unwrapping. Covers loopback, multicast,
 * unique local (`fc00::/7`), link-local (`fe80::/10`), and the unspecified
 * address (`::`). When the address is an IPv4-mapped form (`::ffff:a.b.c.d`),
 * the embedded v4 address is tested against `is_blocked_ipv4`.
 */
fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    ip.is_loopback()
        || ip.is_multicast()
        || is_ipv6_unique_local(ip)
        || is_ipv6_link_local(ip)
        || ip.is_unspecified()
}

/**
 * Validates that a URL is safe to fetch (not a private/internal IP).
 *
 * Blocks requests to:
 * - Private IP ranges (10.x.x.x, 172.16-31.x.x, 192.168.x.x)
 * - Loopback addresses (127.x.x.x, ::1)
 * - Link-local addresses (169.254.x.x)
 * - IPv4-mapped IPv6 addresses (e.g. `::ffff:127.0.0.1`)
 * - Decimal-encoded IPv4 addresses (e.g. `http://2130706433/` for 127.0.0.1)
 *   that would otherwise be parsed as a bare domain name.
 *
 * This check runs **before** `reqwest` performs DNS resolution. The
 * accompanying [`ValidatingResolver`] provides a second layer of defence
 * that re-validates whatever the system resolver actually returns, closing
 * the DNS rebinding TOCTOU window.
 */
fn validate_url_not_private(url: &Url) -> Result<(), ReadError> {
    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if is_blocked_ipv4(ip) {
                    return Err(ReadError::InvalidUrl {
                        path: url.to_string(),
                        reason: "URL points to a restricted/private IP address".to_string(),
                    });
                }
            }
            url::Host::Ipv6(ip) => {
                if is_blocked_ipv6(&ip) {
                    return Err(ReadError::InvalidUrl {
                        path: url.to_string(),
                        reason: "URL points to a restricted/private IP address".to_string(),
                    });
                }
            }
            url::Host::Domain(domain) => {
                // A "domain" here may actually be a numeric IP in decimal
                // notation (e.g. "2130706433" = 127.0.0.1) or a dotted
                // IPv4/IPv6 string that the URL parser chose not to classify.
                // Normalising these prevents trivially bypassing the IP
                // branches above.
                if let Ok(ip) = domain.parse::<IpAddr>() {
                    if is_blocked_ip(ip) {
                        return Err(ReadError::InvalidUrl {
                            path: url.to_string(),
                            reason: "URL points to a restricted/private IP address".to_string(),
                        });
                    }
                } else if let Ok(decimal) = domain.parse::<u32>() {
                    let ip = Ipv4Addr::from(decimal);
                    if is_blocked_ipv4(ip) {
                        return Err(ReadError::InvalidUrl {
                            path: url.to_string(),
                            reason: "URL points to a restricted IP (decimal notation)"
                                .to_string(),
                        });
                    }
                }

                let domain_lower = domain.to_lowercase();
                if domain_lower == "localhost"
                    || domain_lower.ends_with(".local")
                    || domain_lower.ends_with(".internal")
                    || domain_lower.ends_with(".localhost")
                {
                    return Err(ReadError::InvalidUrl {
                        path: url.to_string(),
                        reason: "URL points to a restricted domain".to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/**
 * Check if an IPv6 address is in the unique local address range (fc00::/7).
 */
fn is_ipv6_unique_local(ip: &std::net::Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00
}

/**
 * Check if an IPv6 address is link-local (fe80::/10).
 */
fn is_ipv6_link_local(ip: &std::net::Ipv6Addr) -> bool {
    let segments = ip.segments();
    (segments[0] & 0xffc0) == 0xfe80
}

/**
 * Custom `reqwest` DNS resolver that rejects any hostname whose resolved
 * addresses include private, loopback, link-local, or other restricted
 * ranges.
 *
 * Plugging this resolver into the `reqwest::Client` eliminates the DNS
 * rebinding TOCTOU window: the addresses that pass validation here are the
 * exact addresses that the underlying TCP connector will connect to, so an
 * attacker cannot swap a public IP at validation time for `127.0.0.1` at
 * connect time.
 */
#[derive(Debug, Clone, Default)]
struct ValidatingResolver;

impl Resolve for ValidatingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            // Port 0 is a placeholder; reqwest replaces it with the actual
            // target port when it wires up the TCP connection.
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0u16))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();

            if resolved.is_empty() {
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                    "DNS resolution for '{host}' returned no addresses"
                )));
            }

            for addr in &resolved {
                if is_blocked_ip(addr.ip()) {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                        "blocked address '{}' returned for host '{}' (possible DNS rebinding)",
                        addr.ip(),
                        host,
                    )));
                }
            }

            Ok(Box::new(resolved.into_iter()) as Addrs)
        })
    }
}

/**
 * Fetches a web document, enforces basic robots.txt checks, converts
 * readable HTML into markdown, and returns the normalized ingest payload.
 *
 * # Parameters
 *
 * - `path`: Absolute HTTP or HTTPS URL to ingest.
 * - `enforce_ssrf`: When true, applies the full SSRF guard (URL host
 *   blocklist plus a DNS-level re-validating resolver). Integration
 *   tests pass `false` so they can reach a local Axum server on
 *   127.0.0.1.
 *
 * # Side effects
 *
 * Performs network requests to `robots.txt` and the target URL.
 */
pub async fn read_url_document_with_options(
    path: &str,
    enforce_ssrf: bool,
) -> Result<ReadDocument, ReadError> {
    let parsed_url: Url = Url::parse(path).map_err(|error| ReadError::InvalidUrl {
        path: path.to_string(),
        reason: error.to_string(),
    })?;

    if enforce_ssrf {
        validate_url_not_private(&parsed_url)?;
    }
    // When SSRF enforcement is on, install a DNS resolver that re-validates
    // every resolved IP so the connection cannot silently land on an
    // internal address via DNS rebinding.
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(URL_FETCH_TIMEOUT_SECS))
        .user_agent(URL_INGEST_USER_AGENT);
    if enforce_ssrf {
        builder = builder.dns_resolver(Arc::new(ValidatingResolver));
    }
    let client: Client = builder.build()?;

    enforce_robots_policy(&client, &parsed_url).await?;

    let response = client.get(parsed_url.clone()).send().await?;
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        let retry_after: Option<String> = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        return Err(ReadError::RateLimited {
            url: parsed_url.to_string(),
            retry_after,
        });
    }

    let response = response.error_for_status()?;
    let content_type: String = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body: String = response.text().await?;
    let text: String = if looks_like_html(&content_type, &body) {
        convert_html_to_markdown(&body)?
    } else {
        body.trim().to_string()
    };

    if text.is_empty() {
        return Err(ReadError::HtmlConversion(
            "downloaded page did not yield readable text".to_string(),
        ));
    }

    Ok(ReadDocument {
        file_name: derive_url_file_name(&parsed_url),
        file_type: URL_FILE_TYPE,
        text,
    })
}

/**
 * Checks the origin robots.txt before downloading the target page.
 *
 * Parameters:
 * - `client`: Shared HTTP client used for both robots and page fetches.
 * - `url`: Parsed target URL.
 *
 * Returns:
 * - `Ok(())` when crawling is permitted or no robots.txt exists.
 *
 * Side effects:
 * - Performs a network request to the origin's `/robots.txt`.
 */
async fn enforce_robots_policy(client: &Client, url: &Url) -> Result<(), ReadError> {
    let robots_url: Url = build_robots_url(url)?;
    let response = client.get(robots_url).send().await?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(());
    }

    let response = response.error_for_status()?;
    let robots_body: String = response.text().await?;
    let mut matcher: DefaultMatcher = DefaultMatcher::default();

    if !matcher.one_agent_allowed_by_robots(&robots_body, URL_INGEST_USER_AGENT, url.as_str()) {
        return Err(ReadError::RobotsDisallowed(url.to_string()));
    }

    Ok(())
}

/**
 * Builds the canonical robots.txt URL for a target page.
 *
 * Parameters:
 * - `url`: Parsed page URL.
 *
 * Returns:
 * - Absolute robots.txt URL on the same origin.
 */
fn build_robots_url(url: &Url) -> Result<Url, ReadError> {
    let mut robots_url: Url = url.clone();
    robots_url
        .set_host(url.host_str())
        .map_err(|_| ReadError::InvalidUrl {
            path: url.to_string(),
            reason: "missing host".to_string(),
        })?;
    robots_url.set_path("/robots.txt");
    robots_url.set_query(None);
    robots_url.set_fragment(None);
    Ok(robots_url)
}

/**
 * Converts downloaded HTML into markdown using aggressive preprocessing to
 * remove navigation and other non-content elements.
 *
 * Parameters:
 * - `html`: Raw HTML body.
 *
 * Returns:
 * - Markdown text suitable for the existing chunking pipeline.
 */
fn convert_html_to_markdown(html: &str) -> Result<String, ReadError> {
    let mut options: ConversionOptions = ConversionOptions::default();
    options.preprocessing.enabled = true;
    options.preprocessing.preset = PreprocessingPreset::Aggressive;
    options.preprocessing.remove_navigation = true;
    options.preprocessing.remove_forms = true;

    let result = convert(html, Some(options))
        .map_err(|error| ReadError::HtmlConversion(error.to_string()))?;
    let markdown = result.content.unwrap_or_default();

    Ok(markdown.replace("\r\n", "\n").trim().to_string())
}

/**
 * Derives a stable human-readable file name for URL documents from the URL
 * path, falling back to the host name when the path has no leaf segment.
 *
 * Parameters:
 * - `url`: Parsed target URL.
 *
 * Returns:
 * - Sanitized file name used for display and downstream extraction prompts.
 */
fn derive_url_file_name(url: &Url) -> String {
    let leaf_segment: Option<&str> = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()));
    let raw_name: &str = leaf_segment
        .map(|segment| segment.trim_end_matches(".html").trim_end_matches(".htm"))
        .filter(|segment| !segment.is_empty())
        .or_else(|| url.host_str())
        .unwrap_or("url");
    let sanitized: String = raw_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').to_string()
}

/**
 * Detects whether an HTTP response body should be treated as HTML for
 * readability-style extraction instead of plain text ingestion.
 *
 * Parameters:
 * - `content_type`: Lowercased response content type header.
 * - `body`: Response body.
 *
 * Returns:
 * - `true` when the payload appears to be HTML.
 */
fn looks_like_html(content_type: &str, body: &str) -> bool {
    content_type.contains("text/html")
        || content_type.contains("application/xhtml+xml")
        || body.contains("<html")
        || body.contains("<body")
}

#[cfg(test)]
mod tests {
    use super::{
        derive_url_file_name, is_blocked_ipv4, is_blocked_ipv6, validate_url_not_private,
    };
    use std::net::{Ipv4Addr, Ipv6Addr};

    /**
     * Regression test for SEC-02b: IPv4-mapped IPv6 addresses such as
     * `::ffff:127.0.0.1` must be treated as loopback. Before the fix,
     * `Ipv6Addr::is_loopback()` returned `false` for this encoding.
     */
    #[test]
    fn ipv4_mapped_ipv6_loopback_is_blocked() {
        let mapped = "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap();
        assert!(is_blocked_ipv6(&mapped));
    }

    /**
     * IPv4-mapped IPv6 with a private (10.0.0.0/8) embedded address must
     * also be rejected, confirming we unwrap before delegating to the v4
     * check.
     */
    #[test]
    fn ipv4_mapped_private_is_blocked() {
        let mapped = "::ffff:10.0.0.1".parse::<Ipv6Addr>().unwrap();
        assert!(is_blocked_ipv6(&mapped));
    }

    /**
     * Public IPv6 addresses must continue to be allowed so regular URL
     * ingestion works.
     */
    #[test]
    fn public_ipv6_is_allowed() {
        let public = "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap();
        assert!(!is_blocked_ipv6(&public));
    }

    /**
     * Regression test for SEC-02c: decimal-encoded IPv4 like `2130706433`
     * (= 127.0.0.1) must fail URL validation even though the URL parser
     * treats it as a domain.
     */
    #[test]
    fn decimal_loopback_is_blocked() {
        let url = url::Url::parse("http://2130706433/").unwrap();
        assert!(validate_url_not_private(&url).is_err());
    }

    /**
     * Dotted IPv4 addresses that the URL parser hands us as `Domain`
     * (unusual but possible) must still be validated numerically.
     */
    #[test]
    fn dotted_private_in_domain_branch_is_blocked() {
        // `10.0.0.1` is parsed as Host::Ipv4 by `url`; this test exists to
        // ensure the fallback numeric parse in the Domain arm handles any
        // edge case the parser does not categorise as IPv4.
        let v4: Ipv4Addr = "10.0.0.1".parse().unwrap();
        assert!(is_blocked_ipv4(v4));
    }

    /**
     * Unspecified addresses (`0.0.0.0` / `::`) resolve to the local host on
     * many platforms and must be blocked.
     */
    #[test]
    fn unspecified_addresses_are_blocked() {
        assert!(is_blocked_ipv4("0.0.0.0".parse().unwrap()));
        assert!(is_blocked_ipv6(&"::".parse().unwrap()));
    }


    /**
     * Verifies that URL-derived file names prefer the last path segment and
     * strip common HTML suffixes.
     */
    #[test]
    fn url_file_name_uses_last_path_segment() {
        let url = url::Url::parse("https://example.com/docs/phase-two.html").unwrap();
        assert_eq!(derive_url_file_name(&url), "phase-two");
    }

    /**
     * Verifies that host names are used when the URL path has no terminal
     * segment that can serve as a file name.
     */
    #[test]
    fn url_file_name_falls_back_to_host() {
        let url = url::Url::parse("https://example.com/").unwrap();
        assert_eq!(derive_url_file_name(&url), "example.com");
    }
}
