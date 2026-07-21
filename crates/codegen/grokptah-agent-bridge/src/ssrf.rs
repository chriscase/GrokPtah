//! web_fetch SSRF preflight (#179).
//!
//! Blocks clearly unsafe URL targets before any network I/O.

use std::net::{IpAddr, Ipv4Addr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsrfDecision {
    pub allow: bool,
    pub reason: String,
}

/// Returns whether `url` may be fetched.
pub fn check_url(url: &str) -> SsrfDecision {
    let url = url.trim();
    if url.is_empty() {
        return SsrfDecision {
            allow: false,
            reason: "empty url".into(),
        };
    }
    let lower = url.to_ascii_lowercase();
    let (scheme, rest) = match lower.split_once("://") {
        Some((s, r)) => (s, r),
        None => {
            return SsrfDecision {
                allow: false,
                reason: "missing scheme".into(),
            };
        }
    };
    if scheme != "http" && scheme != "https" {
        return SsrfDecision {
            allow: false,
            reason: format!("scheme {scheme} not allowed"),
        };
    }
    // strip userinfo
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = hostport.split('@').next_back().unwrap_or(hostport);
    let host = if hostport.starts_with('[') {
        hostport
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    check_host(host)
}

fn check_host(host: &str) -> SsrfDecision {
    let h = host.trim().to_ascii_lowercase();
    if h.is_empty() {
        return SsrfDecision {
            allow: false,
            reason: "empty host".into(),
        };
    }
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return SsrfDecision {
            allow: false,
            reason: "blocked host (localhost)".into(),
        };
    }
    if h == "metadata.google.internal" || h == "metadata" {
        return SsrfDecision {
            allow: false,
            reason: "blocked cloud metadata host".into(),
        };
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        if ip_is_blocked(ip) {
            return SsrfDecision {
                allow: false,
                reason: format!("blocked IP {ip}"),
            };
        }
    } else if let Ok(v4) = h.parse::<Ipv4Addr>() {
        if ip_is_blocked(IpAddr::V4(v4)) {
            return SsrfDecision {
                allow: false,
                reason: format!("blocked IP {v4}"),
            };
        }
    }
    SsrfDecision {
        allow: true,
        reason: "ok".into(),
    }
}

fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || (o[0] == 169 && o[1] == 254)
                || o == [169, 254, 169, 254]
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_https_public() {
        let d = check_url("https://example.com/path");
        assert!(d.allow, "{d:?}");
    }

    #[test]
    fn blocks_localhost() {
        assert!(!check_url("http://localhost/admin").allow);
        assert!(!check_url("http://127.0.0.1/").allow);
    }

    #[test]
    fn blocks_metadata() {
        assert!(!check_url("http://169.254.169.254/latest/meta-data").allow);
    }

    #[test]
    fn blocks_private() {
        assert!(!check_url("http://192.168.1.1/").allow);
        assert!(!check_url("http://10.0.0.5/x").allow);
    }

    #[test]
    fn blocks_file_scheme() {
        assert!(!check_url("file:///etc/passwd").allow);
    }
}
