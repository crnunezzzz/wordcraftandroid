//! «Hay una versión nueva»: which published build of this fork is newer than the running one.
//!
//! The `IA Windows` workflow publishes each build as a GitHub release tagged
//! `ia-v<version>-<build>`, with the Windows and Linux zips and (when the signing key is set
//! up) the Android APK. Builds from the workflow carry their number (`CRAFT_IA_BUILD`) and repository
//! (`CRAFT_IA_REPO`); a local build has number 0, so any published build is newer.
//!
//! The hosts fetch [`api_url`] themselves (desktop: HTTPS here; Android: Java) and hand the
//! reply to [`latest`]. «Actualizar» then installs the file: on Windows and Linux the desktop
//! host replaces this copy and reopens it (`desktop::install`); on Android Java hands the APK to the
//! system's installer, which asks the user to confirm.
//!
//! The forks were renamed (`wordcraftandroid` → `wordcraft-asistente`, …, [`RENAMES`]). GitHub
//! answers for both names, but its replies carry the new one: a build accepts release pages and
//! files under either name, so builds made before and after the rename keep updating.
//!
//! The app also says when the original project has published a newer version than this build
//! and no build of this fork carries it yet ([`behind`]): the update is on its way (or the
//! automatic merge needs a look), instead of silence.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Check again after this long (the app may stay open for days).
pub const EVERY_SECS: u64 = 12 * 3600;
/// The release workflow publishes a build whenever the original project changes, which it does
/// nearly every day: a newest build older than this means the automatic merge has stopped (a
/// conflict, a failed test, a disabled workflow) and the app says so instead of staying quiet.
pub const STALE_SECS: u64 = 7 * 86_400;
/// The reply is a short list of releases; anything larger is not GitHub's.
pub const MAX_REPLY: usize = 4 * 1024 * 1024;

/// What the update is for: the file each platform installs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Windows,
    Linux,
    Android,
}
impl Target {
    /// The desktop build this program installs, if one is published for its system.
    pub const DESKTOP: Option<Target> = if cfg!(windows) {
        Some(Target::Windows)
    } else if cfg!(target_os = "linux") {
        Some(Target::Linux)
    } else {
        None
    };

    /// The end of the file name this target installs.
    pub fn suffix(self) -> &'static str {
        match self {
            Target::Windows => "-windows-x64.zip",
            Target::Linux => "-linux-x64.zip",
            Target::Android => ".apk",
        }
    }

    fn wants(self, name: &str) -> bool {
        name.ends_with(self.suffix())
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

/// Each fork's former name and its current one. GitHub redirects the former to the current, and
/// its replies name the current one.
pub const RENAMES: [(&str, &str); 3] = [
    ("crnunezzzz/wordcraftandroid", "crnunezzzz/wordcraft-asistente"),
    ("crnunezzzz/deckcraftandroid", "crnunezzzz/deckcraft-asistente"),
    ("crnunezzzz/gridcraft", "crnunezzzz/gridcraft-asistente"),
];

/// `repo` and, when it was renamed (either way round), its other name.
fn names(repo: &str) -> [&str; 2] {
    let other = RENAMES.iter().find_map(|&(old, new)| {
        if old.eq_ignore_ascii_case(repo) {
            Some(new)
        } else if new.eq_ignore_ascii_case(repo) {
            Some(old)
        } else {
            None
        }
    });
    [repo, other.unwrap_or(repo)]
}

/// `url` is `https://github.com/<repo>/<rest>…` for `repo` under any of its names.
fn under(url: &str, repo: &str, rest: &str) -> bool {
    names(repo).iter().any(|name| {
        url.get(..19).is_some_and(|p| p == "https://github.com/")
            && url.get(19..19 + name.len()).is_some_and(|n| n.eq_ignore_ascii_case(name))
            && url.get(19 + name.len()..).is_some_and(|tail| tail.starts_with(rest))
    })
}

/// The `owner/name` a GitHub release file belongs to (`https://github.com/<owner>/<name>/releases/download/…`).
pub fn repo_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut slashes = rest.match_indices('/').map(|(i, _)| i);
    let _owner_end = slashes.next()?;
    let name_end = slashes.next()?;
    let repo = rest.get(..name_end)?;
    (valid_repo(repo) && rest.get(name_end..).is_some_and(|t| t.starts_with("/releases/"))).then_some(repo)
}

/// GitHub's list of the repository's latest releases (no sign-in needed).
pub fn api_url(repo: &str) -> Option<String> {
    valid_repo(repo).then(|| format!("https://api.github.com/repos/{repo}/releases?per_page=20"))
}

/// The newest published build of `repo` that has a file for `target`, if it is newer than
/// `current`. Drafts, other releases (the original project's) and addresses outside the
/// repository's own release pages are ignored.
pub fn latest(reply: &Value, repo: &str, target: Target, current: u64) -> Option<Release> {
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
        let Some(page) = release["html_url"].as_str().filter(|u| under(u, repo, "/releases/") && safe_url(u)) else { continue };
        let download = release["assets"].as_array().into_iter().flatten().find_map(|asset| {
            let name = asset["name"].as_str()?;
            let url = asset["browser_download_url"].as_str()?;
            (target.wants(name) && under(url, repo, "/releases/download/") && safe_url(url)).then(|| url.to_owned())
        });
        let Some(download) = download else { continue };
        best = Some(Release { build, version: version.to_owned(), page: page.to_owned(), download });
    }
    best
}

/// What one look at the release list found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// A newer build for this platform.
    pub release: Option<Release>,
    /// Why the automatic updates look stopped, when they do ([`stale`]).
    pub stale: Option<String>,
    /// The original project published a newer version that no build of ours carries yet
    /// ([`behind`]); the desktop host fills it in.
    pub behind: Option<String>,
}

/// [`latest`] and [`stale`] from one reply (`now` in Unix seconds).
pub fn check(reply: &Value, repo: &str, target: Target, current: u64, now: u64) -> Checked {
    Checked { release: latest(reply, repo, target, current), stale: stale(reply, repo, target, now), behind: None }
}

/// When `repo`'s newest published build with a file for `target` is older than [`STALE_SECS`],
/// the warning to show: the automatic merge with the original project (or this platform's
/// build) has stopped and needs a look. `None` when a recent build exists, when the list has no
/// such build at all (nothing published yet) or is junk.
pub fn stale(reply: &Value, repo: &str, target: Target, now: u64) -> Option<String> {
    let has_file = |r: &Value| r["assets"].as_array().into_iter().flatten().any(|a| a["name"].as_str().is_some_and(|n| target.wants(n)));
    let newest = reply
        .as_array()?
        .iter()
        .take(100)
        .filter(|r| r["draft"].as_bool() == Some(false) && r["prerelease"].as_bool() != Some(true))
        .filter(|r| r["tag_name"].as_str().is_some_and(|t| t.starts_with("ia-v")))
        .filter(|r| r["html_url"].as_str().is_some_and(|u| under(u, repo, "/releases/")))
        .filter(|r| has_file(r))
        .filter_map(|r| r["published_at"].as_str().and_then(unix_time))
        .max()?;
    let age = now.checked_sub(newest)?;
    if age < STALE_SECS {
        return None;
    }
    let (y, m, d) = civil(newest / 86_400);
    Some(format!(
        "Las actualizaciones automáticas están detenidas: la última versión se publicó el {d:02}/{m:02}/{y} (hace {} días) y el proyecto original sigue avanzando. La fusión necesita revisión: abre github.com/{repo}/actions, flujo «IA Windows».",
        age / 86_400
    ))
}

/// GitHub's address of the original project's newest published release.
pub fn official_url(upstream: &str) -> Option<String> {
    valid_repo(upstream).then(|| format!("https://api.github.com/repos/{upstream}/releases/latest"))
}

/// `0.15.2` or `v0.15.2` as numbers; `None` for anything else (pre-releases included).
fn version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let mut part = || parts.next().filter(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))?.parse::<u64>().ok();
    let v = (part()?, part()?, part()?);
    parts.next().is_none().then_some(v)
}

/// When the original project's newest release (`official`, GitHub's `releases/latest` reply) is
/// newer than this build's `current` version and no build of this fork carries it yet (`newer`,
/// what [`latest`] found), what to tell the user: the official version is out and this fork's
/// build follows once the automatic merge has run. `None` otherwise, or for a junk reply.
pub fn behind(official: &Value, current: &str, newer: Option<&Release>, repo: &str) -> Option<String> {
    if official["draft"].as_bool() == Some(true) || official["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = official["tag_name"].as_str()?;
    let out = version(tag)?;
    if out <= version(current)? || newer.and_then(|r| version(&r.version)).is_some_and(|v| v >= out) {
        return None;
    }
    let (a, b, c) = out;
    Some(format!(
        "Ya salió la versión oficial {a}.{b}.{c}; esta es la {current}. La versión con el asistente se publica sola cuando termina la sincronización automática (normalmente en menos de un día) y aquí verás «Hay una versión nueva». Si tarda más, revisa github.com/{repo}/actions, flujo «IA Windows».",
    ))
}

/// GitHub's `2026-11-02T19:44:19Z` as Unix seconds (`None` for anything else).
fn unix_time(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if b.len() != 20 || !(at(4, b'-') && at(7, b'-') && at(10, b'T') && at(13, b':') && at(16, b':') && at(19, b'Z')) {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<u64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, min, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || min > 59 || s > 60 {
        return None;
    }
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146_097 + doe).checked_sub(719_468)?;
    Some(days * 86_400 + h * 3600 + min * 60 + s)
}

/// Unix days as (year, month, day) (civil_from_days).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + u64::from(m <= 2), m, d)
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
            release(
                "ia-v0.8.1-14",
                &["WordCraft-IA-0.8.1-windows-x64.zip", "WordCraft-IA-0.8.1-Android-arm64.apk", "WordCraft-IA-0.8.1-linux-x64.zip"]
            ),
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
        // Likewise a build without the Linux zip is not an update for Linux.
        let linux = latest(&reply, REPO, Target::Linux, 3).unwrap();
        assert_eq!(linux.build, 14);
        assert!(linux.download.ends_with("-linux-x64.zip"));
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

    fn published(tag: &str, at: &str) -> Value {
        let mut r = release(tag, &["WordCraft-IA-0.8.0-linux-x64.zip"]);
        r["published_at"] = json!(at);
        r
    }

    #[test]
    fn a_week_without_a_new_build_says_the_updates_stopped() {
        let now = unix_time("2026-11-03T12:00:00Z").unwrap();
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_time("2026-10-27T05:16:20Z"), Some(1_793_078_180));
        assert_eq!(civil(1_793_078_180 / 86_400), (2026, 10, 27));
        // The fork's real state on 2026-11-03: one build, from 2026-10-27.
        let reply = json!([published("ia-v0.8.0-1", "2026-10-27T05:16:20Z"), published("android-v0.8.0-android.5", "2026-11-02T00:00:00Z")]);
        let warning = stale(&reply, REPO, Target::Linux, now).unwrap();
        assert!(warning.contains("27/10/2026") && warning.contains("hace 7 días"), "{warning}");
        assert!(warning.contains(&format!("github.com/{REPO}/actions")));
        // A build from yesterday: all is well. Only this fork's own `ia-v` builds count.
        let fresh = json!([published("ia-v0.8.0-1", "2026-10-27T05:16:20Z"), published("ia-v0.15.0-9", "2026-11-02T09:40:00Z")]);
        assert_eq!(stale(&fresh, REPO, Target::Linux, now), None);
        let checked = check(&fresh, REPO, Target::Linux, 1, now);
        assert_eq!((checked.release.map(|r| r.build), checked.stale), (Some(9), None));
        // Linux builds keep coming but none has had an APK for a week: Android is told.
        let mut with_apk = published("ia-v0.8.0-1", "2026-10-20T08:00:00Z");
        with_apk["assets"] = json!([{"name": "WordCraft-IA-0.8.0-Android-arm64.apk"}]);
        let no_new_apk = json!([with_apk, published("ia-v0.15.0-9", "2026-11-02T09:40:00Z")]);
        assert_eq!(stale(&no_new_apk, REPO, Target::Linux, now), None);
        assert!(stale(&no_new_apk, REPO, Target::Android, now).is_some_and(|w| w.contains("20/10/2026")));
    }

    #[test]
    fn no_build_or_junk_dates_raise_no_warning() {
        let now = unix_time("2026-11-03T12:00:00Z").unwrap();
        assert_eq!(stale(&json!([]), REPO, Target::Linux, now), None);
        assert_eq!(stale(&json!({"message": "Not Found"}), REPO, Target::Linux, now), None);
        for junk in ["", "2026-13-01T00:00:00Z", "2026-10-27 05:16:20", "999999-10-27T05:16:20Z", "2026-10-27T05:16:20+01:00", "٢٠٢٦-10-27T05:16:20Z"]
        {
            assert_eq!(stale(&json!([published("ia-v0.8.0-1", junk)]), REPO, Target::Linux, now), None, "{junk:?}");
        }
        // A clock behind the release (a phone set to the wrong date) is not a stopped sync.
        assert_eq!(stale(&json!([published("ia-v0.8.0-1", "2027-01-01T00:00:00Z")]), REPO, Target::Linux, now), None);
        let mut elsewhere = published("ia-v0.8.0-1", "2026-01-01T00:00:00Z");
        elsewhere["html_url"] = json!("https://github.com/attacker/x/releases/tag/ia-v0.8.0-1");
        assert_eq!(stale(&json!([elsewhere]), REPO, Target::Linux, now), None);
    }

    #[test]
    fn builds_from_before_and_after_the_rename_find_each_other() {
        let renamed = "crnunezzzz/wordcraft-asistente";
        let tag = "ia-v0.15.2-13";
        let name = "WordCraft-IA-0.15.2-linux-x64.zip";
        let reply = json!([{
            "tag_name": tag, "draft": false, "prerelease": false, "published_at": "2026-11-06T10:00:00Z",
            "html_url": format!("https://github.com/{renamed}/releases/tag/{tag}"),
            "assets": [{"name": name, "browser_download_url": format!("https://github.com/{renamed}/releases/download/{tag}/{name}")}],
        }]);
        // A build made under the former name, and one under the new name.
        for repo in ["crnunezzzz/wordcraftandroid", renamed] {
            let found = latest(&reply, repo, Target::Linux, 9).unwrap();
            assert_eq!(found.build, 13, "{repo}");
            assert_eq!(repo_of(&found.download), Some(renamed));
        }
        // An unrelated repository's releases are still not ours.
        assert_eq!(latest(&reply, "crnunezzzz/pdfcraftandroid", Target::Linux, 9), None);
        assert_eq!(latest(&reply, "someone/else", Target::Linux, 9), None);
        assert!(!under("https://github.com/crnunezzzz/wordcraft-asistentex/releases/x", renamed, "/releases/"));
        assert_eq!(repo_of("https://github.com/a/b/c"), None);
        assert_eq!(repo_of("https://example.com/a/b/releases/download/x"), None);
    }

    #[test]
    fn a_newer_official_version_without_an_ia_build_is_announced() {
        let official = json!({"tag_name": "v0.15.2", "draft": false, "prerelease": false});
        let notice = behind(&official, "0.15.0", None, REPO).unwrap();
        assert!(notice.contains("0.15.2") && notice.contains("0.15.0") && notice.contains(REPO), "{notice}");
        // Already on it, or newer (a local build), or an IA build carrying it is offered.
        assert_eq!(behind(&official, "0.15.2", None, REPO), None);
        assert_eq!(behind(&official, "0.16.0", None, REPO), None);
        let offered = Release { build: 13, version: "0.15.2".into(), page: String::new(), download: String::new() };
        assert_eq!(behind(&official, "0.15.0", Some(&offered), REPO), None);
        // An IA build of an older version does not cover it.
        let older = Release { version: "0.15.1".into(), ..offered };
        assert!(behind(&official, "0.15.0", Some(&older), REPO).is_some());
        // Pre-releases, drafts and junk say nothing.
        for junk in [
            json!({"tag_name": "v0.16.0-rc.1"}),
            json!({"tag_name": "v0.16.0", "prerelease": true}),
            json!({"tag_name": "v0.16.0", "draft": true}),
            json!({"message": "Not Found"}),
            json!({"tag_name": "v1.2"}),
            json!({"tag_name": "v1.2.3.4"}),
            json!({"tag_name": "v99999999999.0.0"}),
        ] {
            assert_eq!(behind(&junk, "0.15.0", None, REPO), None, "{junk}");
        }
        assert_eq!(official_url("storytold/wordcraft").as_deref(), Some("https://api.github.com/repos/storytold/wordcraft/releases/latest"));
    }

    #[test]
    fn only_plain_repository_names_make_an_address() {
        assert_eq!(api_url("a-b/c.d_e").as_deref(), Some("https://api.github.com/repos/a-b/c.d_e/releases?per_page=20"));
        for bad in ["", "a", "a/b/c", "../x", "a/..", "a/b?x=1", "a /b", "a/b#c"] {
            assert_eq!(api_url(bad), None, "{bad}");
        }
    }
}
