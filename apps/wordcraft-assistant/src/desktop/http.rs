//! HTTPS to the two providers with the user's API key. The request comes from
//! [`crate::protocol::Thread::request`]; the reply has the same shape Android's Java bridge
//! reports, so the agent loop reads both alike: `{status, body}` on success, `{status, detail}`
//! for an HTTP error, `{error}` when no answer came.
#![forbid(unsafe_code)]

use serde_json::{Value, json};
use std::time::Duration;

/// A reply larger than this is refused (the Android bridge has the same limit).
const MAX_REPLY: u64 = 32 * 1024 * 1024;

/// Send one request (blocking: call from a worker thread).
pub fn send(request: &Value, key: &str) -> Value {
    let (Some(url), Some(provider)) = (request["url"].as_str(), request["provider"].as_str()) else {
        return json!({"error":"bad_request"});
    };
    // The key only ever travels to its own provider's API host.
    let host_ok = match provider {
        "openai" => url.starts_with("https://api.openai.com/"),
        "anthropic" => url.starts_with("https://api.anthropic.com/"),
        _ => false,
    };
    if !host_ok {
        return json!({"error":"bad_request"});
    }
    if key.is_empty() {
        return json!({"error":"missing_key"});
    }
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        // Long reasoning with many tool rounds can take minutes; a stalled socket cannot hang
        // the run forever.
        .timeout_global(Some(Duration::from_secs(900)))
        .build()
        .into();
    let mut call = agent.post(url).header("content-type", "application/json");
    if provider == "anthropic" {
        call = call.header("x-api-key", key).header("anthropic-version", "2023-06-01");
        if let Some(beta) = request["beta"].as_str().filter(|b| valid_beta(b)) {
            call = call.header("anthropic-beta", beta);
        }
    } else {
        call = call.header("authorization", &format!("Bearer {key}"));
    }
    match call.send(request["body"].to_string()) {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let text = match response.body_mut().with_config().limit(MAX_REPLY).read_to_string() {
                Ok(text) => text,
                Err(ureq::Error::Timeout(_)) => return json!({"error":"timeout"}),
                Err(_) => return json!({"error":"offline"}),
            };
            if (200..300).contains(&status) {
                match serde_json::from_str::<Value>(&text) {
                    Ok(body) => json!({"status":status,"body":body}),
                    Err(_) => json!({"status":502,"detail":"La respuesta no era JSON válido"}),
                }
            } else {
                let mut detail = text;
                if detail.len() > 6000 {
                    let mut cut = 6000;
                    while !detail.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    detail.truncate(cut);
                }
                json!({"status":status,"detail":detail})
            }
        }
        Err(ureq::Error::Timeout(_)) => json!({"error":"timeout"}),
        Err(_) => json!({"error":"offline"}),
    }
}

/// Beta header values are a short list of lowercase names (as the Android bridge checks).
fn valid_beta(beta: &str) -> bool {
    let names: Vec<&str> = beta.split(',').collect();
    names.len() <= 8
        && names.iter().all(|n| (1..=64).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_never_leave_for_another_host() {
        let to = |provider: &str, url: &str| send(&json!({"provider":provider,"url":url,"body":{}}), "sk-test");
        assert_eq!(to("openai", "https://example.com/v1/responses")["error"], "bad_request");
        assert_eq!(to("anthropic", "https://api.openai.com/v1/responses")["error"], "bad_request");
        assert_eq!(to("other", "https://api.openai.com/v1/responses")["error"], "bad_request");
        assert_eq!(send(&json!({"provider":"openai","url":"https://api.openai.com/v1/responses"}), "")["error"], "missing_key");
        assert!(valid_beta("server-side-fallback-2026-07-01") && !valid_beta("x\r\nevil: 1") && !valid_beta(""));
    }

    /// Reaches the real API (network): a wrong key is a 401 the panel can explain.
    #[test]
    #[ignore]
    fn a_wrong_key_is_reported_as_401() {
        let mut thread = crate::protocol::Thread::new("claude-haiku-5-5");
        thread.push_user("Hola".into(), &[]);
        let request = thread.request("sys", "low", crate::protocol::Degrade::default());
        let event = send(&request, "sk-ant-invalid");
        assert_eq!(event["status"], 401, "{event}");
        let text = crate::protocol::describe_failure(crate::protocol::Provider::Anthropic, "claude-haiku-5-5", &event);
        assert!(text.contains("401"), "{text}");
    }
}
