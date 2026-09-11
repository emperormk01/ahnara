//! Evidence-Preserving Reducer - compress long tool logs via a cheaper model
//! with quote verification. Disabled by default; enable only after review.

use anyhow::Result;

/// Evidence-preserving reduction: takes a long log, asks a cheap model to
/// produce a compact receipt with quoted evidence, then verifies every quote
/// is a substring of the source. If any quote fails, the original is kept.
/// Token win: frontier model reads 300 tokens instead of 10k.
pub async fn try_reduce(long_output: &str) -> Option<String> {
    // Opt-in via env var to avoid surprise token spend or data egress.
    if std::env::var("AHNARA_REDUCER_ENABLED").ok().as_deref() != Some("1") {
        return None;
    }
    let threshold: usize = std::env::var("AHNARA_REDUCER_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8000);
    if long_output.len() < threshold {
        return None;
    }
    let prompt = format!(
        "Compress this tool log into a compact receipt. Keep: failed tests, error messages, and up to 5 quoted evidence lines verbatim under evidenceQuotes. Be concise.\n\nLOG:\n{}",
        &long_output[..long_output.len().min(12000)]
    );
    let url = crate::providers::gateway_endpoint();
    let body = serde_json::json!({
        "model": std::env::var("AHNARA_REDUCER_MODEL").unwrap_or_else(|_| "gemini-3.5-flash-lite".into()),
        "messages": [
            {"role": "system", "content": "You are a concise log summarizer. Output JSON with fields: summary, failedTests, errors, evidenceQuotes (array of verbatim lines from the log)."},
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 800
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let content = v["choices"][0]["message"]["content"].as_str()?;
    // Verify every evidence quote is actually in the source
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(quotes) = parsed.get("evidenceQuotes").and_then(|x| x.as_array()) {
            for q in quotes {
                if let Some(s) = q.as_str() {
                    if !s.is_empty() && !long_output.contains(s) {
                        tracing::warn!("Reducer quote verification failed, discarding receipt");
                        return None;
                    }
                }
            }
        }
        return Some(content.to_string());
    }
    // Fallback: treat content as receipt but verify it contains substrings?
    // If not JSON, ensure at least some overlap to avoid hallucination
    let overlap = content.lines().filter(|l| !l.trim().is_empty() && long_output.contains(l.trim())).count();
    if overlap == 0 && content.len() > 200 {
        return None;
    }
    Some(content.to_string())
}
