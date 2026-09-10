//! Domain code shared by this Petal's route components.
//!
//! Route files under `route/files/` are the controllers: they parse route
//! parameters and bodies, pick the typed shared function, and project the
//! response. Nothing in here inspects route identity.
//!
//! Modules:
//! - `constants` (generated), `policy`: addresses and the day-1 limits.
//! - `abi`, `amount`, `fee`: pure encoders and arithmetic.
//! - `api`: the TOLLY public API (fixed hosts/paths) and its projections.
//! - `chain`: the four allowlisted `bloom:chain` reads, sanitized.
//! - `quote`: best-execution quoting across a token's venues.
//! - `ops`: the durable operation record and its state machine.
//! - `tx`, `swap`, `launch`, `positions`, `wallet`: the write/step flows.
//! - `host`: the only seam to the Bloom host (fake host under `cfg(test)`).

pub mod abi;
pub mod amount;
pub mod api;
pub mod chain;
pub mod constants;
pub mod docs;
pub mod fee;
pub mod host;
pub mod launch;
pub mod ops;
pub mod policy;
pub mod positions;
pub mod quote;
pub mod swap;
pub mod tx;
pub mod wallet;

#[cfg(test)]
mod constants_tests;
#[cfg(test)]
pub mod fake_host;
#[cfg(test)]
mod route_tests;

pub use serde_json;
pub use serde_json::json;

/// Route-file helper: the standard error shape (`-1` not found, `-2` denied,
/// `-3` invalid, `-4` backend).
pub fn err(code: i32, message: impl Into<String>) -> petal::DispatchResponse {
    petal::error(code, message)
}

/// Strip anything that could carry host internals (URLs with keys, provider
/// hostnames) out of a host/upstream error before it reaches a record, a read
/// response, or an error message. Bloom's Arc RPC endpoint carries a secret
/// key in its URL; a transport error can echo that URL.
pub fn sanitize_host_error(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(240));
    let mut rest = message;
    while let Some(pos) = rest.find("://") {
        // Walk back to the scheme start, then forward to the end of the URL.
        let scheme_start = rest[..pos]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        out.push_str(&rest[..scheme_start]);
        out.push_str("<url>");
        let after = &rest[pos + 3..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == ']')
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 240 {
        let truncated: String = collapsed.chars().take(237).collect();
        format!("{truncated}...")
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_host_error;

    #[test]
    fn sanitizer_removes_urls_and_newlines() {
        let raw = "eth_call: error sending request for url (https://arc.example/rpc/SECRETKEY123): connection\nreset";
        let clean = sanitize_host_error(raw);
        assert!(!clean.contains("SECRETKEY123"), "{clean}");
        assert!(!clean.contains("arc.example"), "{clean}");
        assert!(!clean.contains('\n'));
        assert_eq!(
            clean,
            "eth_call: error sending request for url (<url>): connection reset"
        );
    }

    #[test]
    fn sanitizer_truncates_long_messages() {
        let raw = "x".repeat(1000);
        let clean = sanitize_host_error(&raw);
        assert_eq!(clean.chars().count(), 240);
        assert!(clean.ends_with("..."));
    }

    #[test]
    fn sanitizer_keeps_plain_text() {
        assert_eq!(
            sanitize_host_error("execution reverted: Too little received"),
            "execution reverted: Too little received"
        );
    }
}
