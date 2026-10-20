//! «Actualizar» on Windows: download the new build's zip, unpack the program files, and hand
//! them to a small PowerShell helper that waits for this window to close, copies them over this
//! copy and opens it again.
//!
//! The window closes through the app's own close path, so a document with unsaved changes asks
//! first; if the user cancels, the helper keeps waiting and installs when the app is closed.
//! Only the release's own files on GitHub are downloaded (the address [`crate::update::latest`]
//! accepted), and only the program files named below are taken from the zip.
#![forbid(unsafe_code)]

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
    let stem = exe.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).map(str::to_owned);
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
    if !url.starts_with("https://github.com/") || !url.ends_with("-windows-x64.zip") {
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
    let wanted = [format!("{exe}.exe"), format!("{exe}-cli.exe"), "LEEME-IA.txt".to_owned()];
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
        found.push(name);
    }
    if !found.iter().any(|n| n.eq_ignore_ascii_case(&format!("{exe}.exe"))) {
        return Err(format!("La descarga no trae {exe}.exe."));
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

#[cfg(not(windows))]
pub fn launch(_staged: &Path, _exe: &str, _dest: &Path, _report: &Path) -> Result<(), String> {
    Err("La instalación automática es solo para Windows.".into())
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
        std::fs::write(
            &zip,
            zip_with(&[
                ("WordCraft IA/", b""),
                ("WordCraft IA/wordcraft.exe", b"new app"),
                ("WordCraft IA/wordcraft-cli.exe", b"new cli"),
                ("WordCraft IA/LEEME-IA.txt", b"readme"),
                ("WordCraft IA/install-ia.ps1", b"script"),
                ("../wordcraft.exe", b"escape"),
                ("WordCraft IA/other.dll", b"dll"),
            ]),
        )
        .unwrap();
        let dest = unpack(&zip, &dir.join("new"), "wordcraft").unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dest).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["LEEME-IA.txt", "wordcraft-cli.exe", "wordcraft.exe"]);
        assert_eq!(std::fs::read(dest.join("wordcraft.exe")).unwrap(), b"new app");
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
