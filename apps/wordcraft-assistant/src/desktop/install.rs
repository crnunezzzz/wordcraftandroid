//! «Actualizar» on Windows and Linux: download the new build's zip, unpack the program files
//! and put them in place of this copy, then open the app again once this window has closed.
//!
//! - Windows locks a running program's file, so a small PowerShell helper waits for this window
//!   to close, copies the files over this copy and opens it again.
//! - Linux lets a running program's file be replaced (this process keeps the old one until it
//!   exits), so the files are swapped in at once and a small shell helper only waits to reopen.
//!
//! The window closes through the app's own close path, so a document with unsaved changes asks
//! first; if the user cancels, the new build is used the next time the app is opened.
//! Only the release's own files on GitHub are downloaded (the address [`crate::update::latest`]
//! accepted), and only the program files named below are taken from the zip.
#![forbid(unsafe_code)]

use crate::update::Target;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::Duration,
};

/// Larger than any build so far (about 100 MB); anything bigger is not ours.
const MAX_DOWNLOAD: u64 = 600 * 1024 * 1024;
/// Where the helper leaves a failure for the next start to show.
pub const REPORT: &str = "update-error.txt";

/// Whether «Actualizar» installs the build itself here; elsewhere it opens the download.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

/// The files an update replaces: the app, its command line and the readme.
fn program_files(exe: &str) -> [String; 3] {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    [format!("{exe}{ext}"), format!("{exe}-cli{ext}"), "LEEME-IA.txt".to_owned()]
}

/// What the download worker reports.
pub enum Msg {
    Progress(u64, Option<u64>),
    /// The new program files, unpacked into this folder.
    Ready(PathBuf),
    Failed(String),
}

/// The program's own name ("wordcraft") and folder, which the update replaces.
pub fn this_copy() -> Result<(String, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|_| "No se encontró el programa en ejecución.".to_owned())?;
    // Linux names a program whose file was replaced "<path> (deleted)".
    let stem = exe.file_stem().and_then(|s| s.to_str()).map(|s| s.trim_end_matches(" (deleted)")).filter(|s| !s.is_empty()).map(str::to_owned);
    match (stem, exe.parent()) {
        (Some(stem), Some(dir)) => Ok((stem, dir.to_owned())),
        _ => Err("No se encontró el programa en ejecución.".into()),
    }
}

/// Download `url` into `work` and unpack it (blocking: call from a worker thread).
pub fn download(url: &str, work: &Path, exe: &str, tx: &Sender<Msg>) {
    let result = fetch(url, work, tx).and_then(|zip| unpack(&zip, &work.join("new"), exe));
    let _ = tx.send(match result {
        Ok(dir) => Msg::Ready(dir),
        Err(e) => Msg::Failed(e),
    });
}

fn fetch(url: &str, work: &Path, tx: &Sender<Msg>) -> Result<PathBuf, String> {
    if !url.starts_with("https://github.com/") || !Target::DESKTOP.is_some_and(|t| url.ends_with(t.suffix())) {
        return Err("La dirección de la descarga no es de GitHub.".into());
    }
    let offline = |_| "No se pudo descargar la versión nueva: revisa la conexión.".to_owned();
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work).map_err(|e| format!("No se pudo preparar la carpeta de la descarga ({e})."))?;
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(1800)))
        .build()
        .into();
    // GitHub answers with a redirect to its file storage, which ureq follows.
    let mut response = agent.get(url).header("user-agent", "craft-ia-updates").call().map_err(offline)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("GitHub respondió con un error ({status}) al descargar la versión nueva."));
    }
    let total = response.body().content_length().filter(|n| *n > 0);
    if total.is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err("La descarga es demasiado grande para ser esta app.".into());
    }
    let path = work.join("update.zip");
    let mut file = std::fs::File::create(&path).map_err(|e| format!("No se pudo guardar la descarga ({e})."))?;
    let mut reader = response.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    let mut buffer = vec![0u8; 256 * 1024];
    let (mut done, mut reported) = (0u64, 0u64);
    loop {
        let read = reader.read(&mut buffer).map_err(|_| "La descarga se cortó: revisa la conexión y vuelve a intentarlo.".to_owned())?;
        if read == 0 {
            break;
        }
        file.write_all(buffer.get(..read).unwrap_or_default()).map_err(|e| format!("No se pudo guardar la descarga ({e})."))?;
        done = done.saturating_add(read as u64);
        if done - reported >= 512 * 1024 {
            reported = done;
            let _ = tx.send(Msg::Progress(done, total));
        }
    }
    file.flush().map_err(|e| format!("No se pudo guardar la descarga ({e})."))?;
    if total.is_some_and(|n| n != done) {
        return Err("La descarga llegó incompleta: vuelve a intentarlo.".into());
    }
    Ok(path)
}

/// The program files of the build in `zip` (inside its "<App> IA" folder), copied into `dest`.
/// Anything else in the archive is ignored.
fn unpack(zip: &Path, dest: &Path, exe: &str) -> Result<PathBuf, String> {
    let broken = || "El archivo descargado está dañado: vuelve a intentarlo.".to_owned();
    let wanted = program_files(exe);
    let file = std::fs::File::open(zip).map_err(|_| broken())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| broken())?;
    std::fs::create_dir_all(dest).map_err(|e| format!("No se pudo preparar la carpeta de la descarga ({e})."))?;
    let mut found = Vec::new();
    for index in 0..archive.len().min(64) {
        let mut entry = archive.by_index(index).map_err(|_| broken())?;
        let Some(path) = entry.enclosed_name() else { continue };
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if entry.is_dir() || !wanted.iter().any(|w| w.eq_ignore_ascii_case(&name)) || found.contains(&name) || entry.size() > MAX_DOWNLOAD {
            continue;
        }
        let mut out = std::fs::File::create(dest.join(&name)).map_err(|e| format!("No se pudo descomprimir la descarga ({e})."))?;
        std::io::copy(&mut (&mut entry).take(MAX_DOWNLOAD), &mut out).map_err(|_| broken())?;
        #[cfg(unix)]
        if !name.ends_with(".txt") {
            use std::os::unix::fs::PermissionsExt;
            out.set_permissions(std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("No se pudo descomprimir la descarga ({e})."))?;
        }
        found.push(name);
    }
    let [app, ..] = &wanted;
    if !found.iter().any(|n| n.eq_ignore_ascii_case(app)) {
        return Err(format!("La descarga no trae {app}."));
    }
    Ok(dest.to_owned())
}

/// Copies the new files over this copy once the process `pid` has ended, then opens the app
/// again (unless it was closed much later, when the user is likely done with it).
#[cfg_attr(not(windows), allow(dead_code))]
const HELPER: &str = r#"param([int] $ProcessId, [string] $Source, [string] $Dest, [string] $App, [string] $Report)
$started = Get-Date
try { Wait-Process -Id $ProcessId -ErrorAction Stop } catch { }
Start-Sleep -Milliseconds 500
$failed = $null
foreach ($file in "$App.exe", "$App-cli.exe", 'LEEME-IA.txt') {
  $from = Join-Path $Source $file
  if (-not (Test-Path -LiteralPath $from)) { continue }
  $to = Join-Path $Dest $file
  $copied = $false
  for ($try = 0; $try -lt 20 -and -not $copied; $try++) {
    try { Copy-Item -LiteralPath $from -Destination $to -Force -ErrorAction Stop; $copied = $true } catch { Start-Sleep -Milliseconds 500 }
  }
  if (-not $copied) { $failed = $file; break }
  Unblock-File -LiteralPath $to -ErrorAction SilentlyContinue
}
if ($failed) {
  Set-Content -LiteralPath $Report -Encoding UTF8 -Value "No se pudo reemplazar $failed en $Dest. Cierra la app y vuelve a intentarlo, o instala el zip con install-ia.ps1."
}
if (((Get-Date) - $started).TotalMinutes -lt 10) { Start-Process -FilePath (Join-Path $Dest "$App.exe") -WorkingDirectory $Dest }
Remove-Item -LiteralPath (Split-Path -Parent $Source) -Recurse -Force -ErrorAction SilentlyContinue
"#;

/// Start the helper for the files in `staged`. The caller then closes the window.
#[cfg(windows)]
pub fn launch(staged: &Path, exe: &str, dest: &Path, report: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let failed = |e: std::io::Error| format!("No se pudo iniciar la instalación ({e}).");
    let work = staged.parent().unwrap_or(staged);
    let script = work.join("instalar.ps1");
    // A BOM so Windows PowerShell 5 reads the script as UTF-8.
    std::fs::write(&script, [&[0xEF, 0xBB, 0xBF][..], HELPER.as_bytes()].concat()).map_err(failed)?;
    let start = |flags: u32| {
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
            .arg(&script)
            .arg("-ProcessId")
            .arg(std::process::id().to_string())
            .arg("-Source")
            .arg(staged)
            .arg("-Dest")
            .arg(dest)
            .arg("-App")
            .arg(exe)
            .arg("-Report")
            .arg(report)
            .creation_flags(flags)
            .spawn()
    };
    // Out of the app's job when allowed, so closing the app cannot end the helper with it.
    start(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| start(CREATE_NO_WINDOW)).map(drop).map_err(failed)
}

/// Waits for the process `$1` to end and opens `$3/$2` again (unless that was much later,
/// when the user is likely done with it).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const REOPEN: &str = r#"started=$(date +%s)
while kill -0 "$1" 2>/dev/null; do sleep 0.5; done
if [ $(( $(date +%s) - started )) -lt 600 ]; then cd "$3" && exec "./$2"; fi"#;

/// Put the files in `staged` in place of this copy and start the helper that reopens it. The
/// caller then closes the window.
#[cfg(target_os = "linux")]
pub fn launch(staged: &Path, exe: &str, dest: &Path, _report: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let names: Vec<String> = program_files(exe).into_iter().filter(|name| staged.join(name).is_file()).collect();
    // Every file is copied beside its target first and then renamed over it, so a failure
    // (a full disk, a folder the user cannot write) leaves this copy as it was.
    let temp = |name: &str| dest.join(format!(".{name}.update"));
    let cleanup = || names.iter().for_each(|name| drop(std::fs::remove_file(temp(name))));
    for name in &names {
        if let Err(e) = std::fs::copy(staged.join(name), temp(name)) {
            cleanup();
            return Err(format!("No se pudo copiar la versión nueva en {} ({e}). Instálala con install-ia.sh desde el zip.", dest.display()));
        }
    }
    for name in &names {
        if let Err(e) = std::fs::rename(temp(name), dest.join(name)) {
            cleanup();
            return Err(format!("No se pudo reemplazar {name} en {} ({e}). Instálala con install-ia.sh desde el zip.", dest.display()));
        }
    }
    let _ = std::fs::remove_dir_all(staged.parent().unwrap_or(staged));
    // In a process group of its own, so closing the app cannot end the helper with it.
    std::process::Command::new("sh")
        .args(["-c", REOPEN, "sh"])
        .arg(std::process::id().to_string())
        .arg(exe)
        .arg(dest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
        // The new build is already in place: it opens the next time the app is started.
        .or(Ok(()))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn launch(_staged: &Path, _exe: &str, _dest: &Path, _report: &Path) -> Result<(), String> {
    Err("La instalación automática es solo para Windows y Linux.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            out.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            out.write_all(bytes).unwrap();
        }
        out.finish().unwrap().into_inner()
    }

    #[test]
    fn only_the_program_files_are_unpacked() {
        let dir = std::env::temp_dir().join(format!("craft-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("update.zip");
        let [app, cli, readme] = program_files("wordcraft");
        let inside = |name: &str| format!("WordCraft IA/{name}");
        std::fs::write(
            &zip,
            zip_with(&[
                ("WordCraft IA/", b""),
                (&inside(&app), b"new app"),
                (&inside(&cli), b"new cli"),
                (&inside(&readme), b"readme"),
                ("WordCraft IA/install-ia.ps1", b"script"),
                ("WordCraft IA/install-ia.sh", b"script"),
                (&format!("../{app}"), b"escape"),
                ("WordCraft IA/other.dll", b"dll"),
            ]),
        )
        .unwrap();
        let dest = unpack(&zip, &dir.join("new"), "wordcraft").unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        let mut expected = vec![app.clone(), cli, readme];
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(std::fs::read(dest.join(&app)).unwrap(), b"new app");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dest.join(&app)).unwrap().permissions().mode() & 0o111, 0o111, "the program can run");
        }
        // Another app's zip is refused rather than half-installed.
        assert!(unpack(&zip, &dir.join("other"), "deckcraft").is_err());
        std::fs::write(&zip, b"not a zip").unwrap();
        assert!(unpack(&zip, &dir.join("broken"), "wordcraft").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_helper_takes_what_launch_passes() {
        for param in ["[int] $ProcessId", "[string] $Source", "[string] $Dest", "[string] $App", "[string] $Report"] {
            assert!(HELPER.starts_with("param(") && HELPER.lines().next().unwrap().contains(param), "{param}");
        }
        // The files it copies are the ones `unpack` keeps.
        assert!(HELPER.contains(r#""$App.exe", "$App-cli.exe", 'LEEME-IA.txt'"#));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_swaps_the_files_in_and_reopens_after_exit() {
        let dir = std::env::temp_dir().join(format!("craft-install-linux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (staged, dest) = (dir.join("work").join("new"), dir.join("WordCraft IA"));
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(staged.join("wordcraft"), b"new app").unwrap();
        std::fs::write(staged.join("LEEME-IA.txt"), b"new readme").unwrap();
        std::fs::write(dest.join("wordcraft"), b"old app").unwrap();
        std::fs::write(dest.join("wordcraft-cli"), b"old cli").unwrap();
        launch(&staged, "wordcraft", &dest, &dir.join(REPORT)).unwrap();
        assert_eq!(std::fs::read(dest.join("wordcraft")).unwrap(), b"new app");
        assert_eq!(std::fs::read(dest.join("LEEME-IA.txt")).unwrap(), b"new readme");
        // A file the build does not bring stays as it was; no temporary files are left behind.
        assert_eq!(std::fs::read(dest.join("wordcraft-cli")).unwrap(), b"old cli");
        let mut names: Vec<_> = std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["LEEME-IA.txt", "wordcraft", "wordcraft-cli"]);
        assert!(!dir.join("work").exists(), "the download is cleaned up");
        // A folder that cannot be written leaves the old copy and says so.
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("wordcraft"), b"newer app").unwrap();
        assert!(launch(&staged, "wordcraft", &dir.join("missing"), &dir.join(REPORT)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reopen_helper_waits_for_the_app_to_end() {
        // $1 the process, $2 the program, $3 its folder: the order `launch` passes them.
        assert!(REOPEN.contains(r#"kill -0 "$1""#) && REOPEN.contains(r#"cd "$3" && exec "./$2""#));
    }

    #[test]
    fn only_github_release_zips_are_downloaded() {
        let (tx, rx) = std::sync::mpsc::channel();
        let work = std::env::temp_dir().join(format!("craft-install-url-{}", std::process::id()));
        for url in
            ["http://github.com/a/b/releases/download/x/W-windows-x64.zip", "https://example.com/W-windows-x64.zip", "https://github.com/a/b/x.exe"]
        {
            download(url, &work, "wordcraft", &tx);
            assert!(matches!(rx.recv().unwrap(), Msg::Failed(_)), "{url}");
        }
        let _ = std::fs::remove_dir_all(&work);
    }
}
