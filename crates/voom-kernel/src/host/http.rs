//! HTTP host functions.

use std::io::Read as _;

use crate::host::{HostState, HttpResponse};

/// Extract the host (domain) from a URL string.
/// Supports `scheme://[user@]host[:port]/...` forms.
fn extract_url_host(url: &str) -> Result<String, String> {
    let after_scheme = url
        .find("://")
        .map(|i| &url[i + 3..])
        .ok_or_else(|| format!("invalid URL '{url}': missing scheme"))?;
    let after_userinfo = after_scheme
        .find('@')
        .map_or(after_scheme, |i| &after_scheme[i + 1..]);
    let authority = after_userinfo
        .find(['/', '?', '#'])
        .map_or(after_userinfo, |i| &after_userinfo[..i]);
    let host = if authority.starts_with('[') {
        authority.find(']').map_or(authority, |i| &authority[..=i])
    } else {
        authority.rfind(':').map_or(authority, |i| &authority[..i])
    };
    if host.is_empty() {
        return Err(format!("URL '{url}' has no host"));
    }
    Ok(host.to_string())
}

impl HostState {
    /// Check that the URL's domain is in the allowed HTTP domains list.
    /// Empty allowlist = deny all (matches `run_tool` semantics).
    fn check_http_domain(&self, url: &str) -> Result<(), String> {
        if self.allowed_http_domains.is_empty() {
            return Err(format!(
                "HTTP access not enabled for plugin '{}' (no allowed domains)",
                self.plugin_name
            ));
        }
        let domain = extract_url_host(url)?;
        let allowed = self.allowed_http_domains.iter().any(|d| d == &domain);
        if !allowed {
            return Err(format!(
                "domain '{domain}' is not in the allowed list for plugin '{}'",
                self.plugin_name
            ));
        }
        Ok(())
    }

    /// Perform an HTTP GET request.
    pub fn http_get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<HttpResponse, String> {
        self.require_capability_kind("serve_http", "HTTP GET")?;
        self.check_http_domain(url)?;

        let mut request = ureq::get(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .call()
            .map_err(|e| format!("HTTP GET failed: {e}"))?;

        parse_response(response)
    }

    /// Perform an HTTP POST request.
    pub fn http_post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, String> {
        self.require_capability_kind("serve_http", "HTTP POST")?;
        self.check_http_domain(url)?;

        let mut request = ureq::post(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }

        let response = request
            .send_bytes(body)
            .map_err(|e| format!("HTTP POST failed: {e}"))?;

        parse_response(response)
    }
}

/// Extract status, headers, and body from a ureq response.
fn parse_response(response: ureq::Response) -> Result<HttpResponse, String> {
    let status = response.status();
    let header_names = response.headers_names();
    let headers: Vec<(String, String)> = header_names
        .iter()
        .filter_map(|name| {
            response
                .header(name)
                .map(|val| (name.clone(), val.to_string()))
        })
        .collect();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(10 * 1024 * 1024)
        .read_to_end(&mut body)
        .map_err(|e| format!("failed to read response body: {e}"))?;

    Ok(HttpResponse::with_headers(status, headers, body))
}

#[cfg(test)]
mod tests {
    use crate::host::HostState;

    use super::extract_url_host;

    #[test]
    fn test_extract_url_host_basic() {
        assert_eq!(
            extract_url_host("http://example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://api.example.com:443/v1").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            extract_url_host("http://192.0.2.1:8080").unwrap(),
            "192.0.2.1"
        );
    }

    #[test]
    fn test_extract_url_host_no_scheme() {
        assert!(extract_url_host("example.com/path").is_err());
    }

    #[test]
    fn test_extract_url_host_strips_userinfo() {
        assert_eq!(
            extract_url_host("http://user:pass@example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://alice@api.example.com/v1").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            extract_url_host("http://user:pass@example.com:8080/x").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_bracketed_ipv6() {
        assert_eq!(extract_url_host("http://[::1]/path").unwrap(), "[::1]");
        assert_eq!(
            extract_url_host("http://[2001:db8::1]:8080/x").unwrap(),
            "[2001:db8::1]"
        );
    }

    #[test]
    fn test_extract_url_host_ignores_query_and_fragment() {
        assert_eq!(
            extract_url_host("http://example.com/p?foo=bar").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("http://example.com/p#frag").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("http://example.com?x=1").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("http://example.com#frag").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_no_path() {
        assert_eq!(
            extract_url_host("http://example.com").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_url_host("https://example.com:443").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn test_extract_url_host_empty_host_rejected() {
        let err = extract_url_host("http:///path").unwrap_err();
        assert!(
            err.contains("no host"),
            "expected empty-host rejection: {err}"
        );
        let err = extract_url_host("http://user@/path").unwrap_err();
        assert!(
            err.contains("no host"),
            "expected empty-host rejection: {err}"
        );
    }

    #[test]
    fn test_check_http_domain_allowed_host_passes() {
        let state = HostState::new("test".into()).with_http_domains(vec!["api.example.com".into()]);
        assert!(state
            .check_http_domain("https://api.example.com/v1/resource")
            .is_ok());
    }

    #[test]
    fn test_check_http_domain_host_not_on_allowlist() {
        let state = HostState::new("test".into()).with_http_domains(vec!["api.example.com".into()]);
        let err = state
            .check_http_domain("https://other.example.com/x")
            .unwrap_err();
        assert!(err.contains("not in the allowed list"), "got: {err}");
    }

    #[test]
    fn test_check_http_domain_suffix_injection_rejected() {
        let state = HostState::new("test".into()).with_http_domains(vec!["example.com".into()]);
        let err = state
            .check_http_domain("http://example.com.evil.net/path")
            .unwrap_err();
        assert!(
            err.contains("not in the allowed list"),
            "suffix injection must be rejected, got: {err}"
        );
        let err = state
            .check_http_domain("http://evilexample.com/")
            .unwrap_err();
        assert!(
            err.contains("not in the allowed list"),
            "prefix injection must be rejected, got: {err}"
        );
    }

    #[test]
    fn test_check_http_domain_invalid_url_rejected() {
        let state = HostState::new("test".into()).with_http_domains(vec!["example.com".into()]);
        let err = state.check_http_domain("not-a-url").unwrap_err();
        assert!(
            err.contains("missing scheme") || err.contains("invalid URL"),
            "expected URL-parse error, got: {err}"
        );
    }

    #[test]
    fn test_check_http_domain_empty_allowlist_denies() {
        let state = HostState::new("test".into());
        let err = state.check_http_domain("http://example.com").unwrap_err();
        assert!(
            err.contains("no allowed domains"),
            "empty allowlist must deny, got: {err}"
        );
    }
}
