//! «Hay una versión nueva»: which published build of this fork is newer than the running one.
//!
//! The `IA Windows` workflow publishes each build as a GitHub release tagged
//! `ia-v<version>-<build>`, with the Windows zip and (when the signing key is set up) the
//! Android APK. Builds from the workflow carry their number (`CRAFT_IA_BUILD`) and repository
//! (`CRAFT_IA_REPO`); a local build has number 0, so any published build is newer.
//!
//! The hosts fetch [`api_url`] themselves (desktop: HTTPS here; Android: Java) and hand the
//! reply to [`latest`]. «Actualizar» then installs the file: on Windows the desktop host
//! replaces this copy and reopens it (`desktop::install`); on Android Java hands the APK to the
//! system's installer, which asks the user to confirm.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Check again after this long (the app may stay open for days).
pub const EVERY_SECS: u64 = 12 * 3600;
/// The reply is a short list of releases; anything larger is not GitHub's.
pub const MAX_REPLY: usize = 4 * 1024 * 1024;

/// What the update is for: the file each platform installs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Windows,
    Android,
}
impl Target {
    fn wants(self, name: &str) -> bool {
        match self {
            Target::Windows => name.ends_with("-windows-x64.zip"),
            Target::Android => name.ends_with(".apk"),
        }
    }
}

/// A published build newer than this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub build: u64,
    /// The app version it was built from ("0.8.0").
    pub version: String,
    /// The release's page on GitHub.
    pub page: String,
    /// The file to install on this platform.
    pub download: String,
}

/// This build's number from the workflow; 0 for a local build.
pub fn build() -> u64 {
    option_env!("CRAFT_IA_BUILD").and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

/// The repository whose releases are this app's updates: the workflow's own, else `default`.
pub fn repo(default: &'static str) -> &'static str {
    match option_env!("CRAFT_IA_REPO") {
        Some(repo) if valid_repo(repo) => repo,
        _ => default,
    }
}

fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty() && p.len() <= 100 && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && p != "." && p != ".."
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// GitHub's list of the repository's latest releases (no sign-in needed).
pub fn api_url(repo: &str) -> Option<String> {
    valid_repo(repo).then(|| format!("https://api.github.com/repos/{repo}/releases?per_page=20"))
}

/// The newest published build of `repo` that has a file for `target`, if it is newer than
/// `current`. Drafts, other releases (the original project's) and addresses outside the
/// repository's own release pages are ignored.
pub fn latest(reply: &Value, repo: &str, target: Target, current: u64) -> Option<Release> {
    let pages = format!("https://github.com/{repo}/releases/");
    let files = format!("https://github.com/{repo}/releases/download/");
    let mut best: Option<Release> = None;
    for release in reply.as_array()?.iter().take(100) {
        if release["draft"].as_bool() != Some(false) || release["prerelease"].as_bool() == Some(true) {
            continue;
        }
        let Some(rest) = release["tag_name"].as_str().and_then(|t| t.strip_prefix("ia-v")) else { continue };
        let Some((version, build)) = rest.rsplit_once('-') else { continue };
        let Ok(build) = build.parse::<u64>() else { continue };
        if build <= current || best.as_ref().is_some_and(|b| b.build >= build) || version.is_empty() || version.len() > 40 {
            continue;
        }
        let Some(page) = release["html_url"].as_str().filter(|u| u.starts_with(&pages) && safe_url(u)) else { continue };
        let download = release["assets"].as_array().into_iter().flatten().find_map(|asset| {
            let name = asset["name"].as_str()?;
            let url = asset["browser_download_url"].as_str()?;
            (target.wants(name) && url.starts_with(&files) && safe_url(url)).then(|| url.to_owned())
        });
        let Some(download) = download else { continue };
        best = Some(Release { build, version: version.to_owned(), page: page.to_owned(), download });
    }
    best
}

/// Why a check found nothing, in the settings' words. `None`: no answer at all.
pub fn failure(status: Option<u16>) -> String {
    match status {
        None => "No se pudo consultar GitHub: revisa la conexión. Se volverá a intentar más tarde.".into(),
        Some(403 | 429) => "GitHub limitó las consultas por ahora. Se volverá a intentar más tarde.".into(),
        Some(404) => "No se encontró el repositorio de las versiones.".into(),
        Some(status) => format!("GitHub respondió con un error ({status}). Se volverá a intentar más tarde."),
    }
}

fn safe_url(url: &str) -> bool {
    url.len() <= 500 && url.chars().all(|c| c.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REPO: &str = "someone/wordcraftandroid";

    fn release(tag: &str, assets: &[&str]) -> Value {
        json!({
            "tag_name": tag, "draft": false, "prerelease": false,
            "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "assets": assets.iter().map(|name| json!({
                "name": name,
                "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_newest_build_with_a_file_for_the_platform_wins() {
        let reply = json!([
            release("ia-v0.8.1-12", &["WordCraft-IA-0.8.1-windows-x64.zip"]),
            release("ia-v0.8.1-14", &["WordCraft-IA-0.8.1-windows-x64.zip", "WordCraft-IA-0.8.1-Android-arm64.apk"]),
            release("ia-v0.8.1-15", &["WordCraft-IA-0.8.1-windows-x64.zip"]),
            release("android-v0.9.0-android.1", &["WordCraft-Android-arm64.apk"]),
        ]);
        let windows = latest(&reply, REPO, Target::Windows, 3).unwrap();
        assert_eq!((windows.build, windows.version.as_str()), (15, "0.8.1"));
        assert!(windows.download.ends_with("/ia-v0.8.1-15/WordCraft-IA-0.8.1-windows-x64.zip"));
        // A build without an APK (no signing key that day) is not an update for Android.
        let android = latest(&reply, REPO, Target::Android, 3).unwrap();
        assert_eq!(android.build, 14);
        assert!(android.download.ends_with(".apk"));
        assert_eq!(latest(&reply, REPO, Target::Windows, 15), None);
        assert_eq!(latest(&reply, REPO, Target::Android, 14), None);
    }

    #[test]
    fn drafts_foreign_addresses_and_junk_are_ignored() {
        let mut draft = release("ia-v0.8.1-20", &["WordCraft-IA-0.8.1-windows-x64.zip"]);
        draft["draft"] = json!(true);
        let mut elsewhere = release("ia-v0.8.1-21", &["WordCraft-IA-0.8.1-windows-x64.zip"]);
        elsewhere["assets"][0]["browser_download_url"] = json!("https://example.com/WordCraft-IA-0.8.1-windows-x64.zip");
        let mut other_repo = release("ia-v0.8.1-22", &["WordCraft-IA-0.8.1-windows-x64.zip"]);
        other_repo["html_url"] = json!("https://github.com/attacker/wordcraftandroid/releases/tag/ia-v0.8.1-22");
        let reply = json!([draft, elsewhere, other_repo, release("ia-v-x", &["a-windows-x64.zip"]), 7, null, {"tag_name": 3}]);
        assert_eq!(latest(&reply, REPO, Target::Windows, 0), None);
        assert_eq!(latest(&json!({"message": "API rate limit exceeded"}), REPO, Target::Windows, 0), None);
    }

    #[test]
    fn only_plain_repository_names_make_an_address() {
        assert_eq!(api_url("a-b/c.d_e").as_deref(), Some("https://api.github.com/repos/a-b/c.d_e/releases?per_page=20"));
        for bad in ["", "a", "a/b/c", "../x", "a/..", "a/b?x=1", "a /b", "a/b#c"] {
            assert_eq!(api_url(bad), None, "{bad}");
        }
    }
}
