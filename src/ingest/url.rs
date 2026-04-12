use std::time::Duration;

use html_to_markdown_rs::{ConversionOptions, PreprocessingPreset, convert};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use robotstxt::DefaultMatcher;
use url::Url;

use super::reader::{ReadDocument, ReadError, URL_FILE_TYPE};

const URL_INGEST_USER_AGENT: &str = "wend-rag/0.1";
const URL_FETCH_TIMEOUT_SECS: u64 = 30;

/**
 * Validates that a URL is safe to fetch (not a private/internal IP).
 *
 * Blocks requests to:
 * - Private IP ranges (10.x.x.x, 172.16-31.x.x, 192.168.x.x)
 * - Loopback addresses (127.x.x.x, ::1)
 * - Link-local addresses (169.254.x.x)
 *
 * Parameters:
 * - `url`: The parsed URL to validate.
 *
 * Returns:
 * - `Ok(())` if the URL is safe to fetch.
 * - `Err(ReadError)` if the URL points to a restricted address.
 */
fn validate_url_not_private(url: &Url) -> Result<(), ReadError> {
    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_multicast()
                    || ip.is_broadcast()
                    || ip.is_documentation()
                {
                    return Err(ReadError::InvalidUrl {
                        path: url.to_string(),
                        reason: "URL points to a restricted/private IP address".to_string(),
                    });
                }
            }
            url::Host::Ipv6(ip) => {
                if ip.is_loopback()
                    || ip.is_multicast()
                    || is_ipv6_unique_local(&ip)
                    || is_ipv6_link_local(&ip)
                {
                    return Err(ReadError::InvalidUrl {
                        path: url.to_string(),
                        reason: "URL points to a restricted/private IP address".to_string(),
                    });
                }
            }
            url::Host::Domain(domain) => {
                // Block localhost and common internal domains
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
 * Fetches a web document, enforces basic robots.txt checks, converts readable
 * HTML into markdown, and returns the normalized ingest payload.
 *
 * Parameters:
 * - `path`: Absolute HTTP or HTTPS URL to ingest.
 *
 * Returns:
 * - `ReadDocument` containing the original URL-derived file name, `url`
 *   file type, and normalized markdown/text content.
 *
 * Side effects:
 * - Performs network requests to `robots.txt` and the target URL.
 */
pub async fn read_url_document(path: &str) -> Result<ReadDocument, ReadError> {
    let parsed_url: Url = Url::parse(path).map_err(|error| ReadError::InvalidUrl {
        path: path.to_string(),
        reason: error.to_string(),
    })?;

    // SECURITY: Validate URL doesn't point to private/internal addresses (SSRF protection)
    validate_url_not_private(&parsed_url)?;
    let client: Client = Client::builder()
        .timeout(Duration::from_secs(URL_FETCH_TIMEOUT_SECS))
        .user_agent(URL_INGEST_USER_AGENT)
        .build()?;

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
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back());
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
    use super::derive_url_file_name;

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
