use std::process::Command;
use std::time::Duration;

use crate::project::{CURRENT_VERSION, GITHUB_REPO};

#[derive(Clone)]
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Available { version: String, url: String },
    Skipped { version: String, url: String },
    Failed(String),
}

pub fn check_for_update() -> UpdateStatus {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let response = match agent
        .get(&url)
        .set("User-Agent", "interceptor-update-check")
        .call()
    {
        Ok(response) => response,
        Err(error) => return UpdateStatus::Failed(error.to_string()),
    };

    let body = match response.into_string() {
        Ok(body) => body,
        Err(error) => return UpdateStatus::Failed(error.to_string()),
    };

    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(error) => return UpdateStatus::Failed(error.to_string()),
    };

    let latest = match json["tag_name"].as_str() {
        Some(tag) => tag.trim_start_matches('v').to_string(),
        None => return UpdateStatus::Failed("latest release has no tag_name".to_string()),
    };

    let page_url = json["html_url"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(github_releases_url);

    update_status_for_versions(CURRENT_VERSION, &latest, page_url)
}

pub fn update_available(status: &UpdateStatus) -> bool {
    matches!(status, UpdateStatus::Available { .. })
}

pub fn open_update_page(status: &UpdateStatus) {
    let url = match status {
        UpdateStatus::Available { url, .. } | UpdateStatus::Skipped { url, .. } => url.clone(),
        _ => github_releases_url(),
    };

    Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn()
        .ok();
}

fn github_releases_url() -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/latest")
}

fn update_status_for_versions(
    running_version: &str,
    latest_version: &str,
    url: String,
) -> UpdateStatus {
    let running = match semver::Version::parse(running_version.trim_start_matches('v')) {
        Ok(version) => version,
        Err(error) => return UpdateStatus::Failed(format!("invalid app version: {error}")),
    };
    let latest = match semver::Version::parse(latest_version.trim_start_matches('v')) {
        Ok(version) => version,
        Err(error) => return UpdateStatus::Failed(format!("invalid release version: {error}")),
    };

    if latest > running {
        UpdateStatus::Available {
            version: latest_version.trim_start_matches('v').to_string(),
            url,
        }
    } else {
        UpdateStatus::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_release_is_offered() {
        let status = update_status_for_versions(
            "0.1.0",
            "v0.2.0",
            "https://example.invalid/release".to_string(),
        );

        assert!(matches!(
            status,
            UpdateStatus::Available { version, .. } if version == "0.2.0"
        ));
    }

    #[test]
    fn older_release_is_not_presented_as_an_update() {
        let status = update_status_for_versions(
            "0.2.0",
            "v0.1.0",
            "https://example.invalid/release".to_string(),
        );

        assert!(matches!(status, UpdateStatus::UpToDate));
    }

    #[test]
    fn malformed_release_version_does_not_create_an_update_prompt() {
        let status = update_status_for_versions(
            "0.1.0",
            "today-build",
            "https://example.invalid/release".to_string(),
        );

        assert!(matches!(status, UpdateStatus::Failed(_)));
    }
}
