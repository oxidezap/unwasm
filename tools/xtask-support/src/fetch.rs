//! HTTP downloads with explicit credential and redirect boundaries.
use anyhow::{Context, Result, ensure};
use std::io::Read;

/// Discover an existing GitHub token without prompting or logging it.
pub fn github_token() -> Option<String> {
    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

/// Download a URL. Credentials are sent only to api.github.com and removed on every redirect.
pub fn get(address: &str, accept: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut url = url::Url::parse(address)?;
    ensure!(
        token.is_none() || (url.scheme() == "https" && url.host_str() == Some("api.github.com")),
        "GitHub credentials cannot be sent to another host"
    );
    let mut auth = token;
    for _ in 0..10 {
        let mut request = agent
            .get(url.as_str())
            .header("Accept", accept)
            .header("User-Agent", "cargo-xt");
        if let Some(token) = auth {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request.call().context("HTTP request failed")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .context("redirect without Location")?
                .to_str()?;
            url = url.join(location)?;
            ensure!(
                url.scheme() == "https" || auth.is_none(),
                "authenticated download redirected to plaintext"
            );
            auth = None;
            continue;
        }
        ensure!(
            response.status().is_success(),
            "HTTP {} from {}",
            response.status(),
            url.host_str().unwrap_or("unknown host")
        );
        let mut bytes = Vec::new();
        response.into_body().into_reader().read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    anyhow::bail!("too many redirects")
}

#[cfg(test)]
mod tests {
    #[test]
    fn credentials_are_refused_outside_the_github_api_before_any_io() {
        assert!(
            super::get(
                "https://example.invalid/",
                "application/json",
                Some("test-token")
            )
            .is_err()
        );
        assert!(
            super::get(
                "http://api.github.com/",
                "application/json",
                Some("test-token")
            )
            .is_err()
        );
    }
}
