//! Puts a downloaded release file on disk the way its type expects:
//! - Windows setup `.exe` (Inno Setup) runs silently, per user, into `current/`
//! - portable `.exe`, `.zip` and `.tar.gz` are unpacked into `current/`
//! - `.deb` is installed system-wide through apt (asks for the password)
//! - `.dmg` has its `.app` copied to the Applications folder

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Kind of release file, recognised by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    Zip,
    TarGz,
    /// Installer program (`*setup*.exe` / `*install*.exe`).
    Setup,
    /// Portable program.
    Exe,
    Deb,
    Dmg,
}

impl Package {
    pub fn of(name: &str) -> Option<Self> {
        let n = name.to_lowercase();
        Some(if n.ends_with(".zip") {
            Self::Zip
        } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
            Self::TarGz
        } else if n.ends_with(".exe") {
            if n.contains("setup") || n.contains("install") { Self::Setup } else { Self::Exe }
        } else if n.ends_with(".deb") {
            Self::Deb
        } else if n.ends_with(".dmg") {
            Self::Dmg
        } else {
            return None;
        })
    }

    /// What this computer can install, most preferred first.
    pub fn preferred() -> &'static [Package] {
        if cfg!(windows) {
            &[Self::Setup, Self::Exe, Self::Zip]
        } else if cfg!(target_os = "macos") {
            &[Self::Dmg, Self::Zip, Self::TarGz]
        } else if Path::new("/usr/bin/dpkg").exists() {
            &[Self::Deb, Self::TarGz, Self::Zip]
        } else {
            &[Self::TarGz, Self::Zip]
        }
    }

    /// Archives work on any OS, so their name has to say which one.
    pub fn is_archive(self) -> bool {
        matches!(self, Self::Zip | Self::TarGz)
    }

    /// Name to save the download as (installers need the right extension).
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Zip => "download.zip",
            Self::TarGz => "download.tar.gz",
            Self::Setup | Self::Exe => "download.exe",
            Self::Deb => "download.deb",
            Self::Dmg => "download.dmg",
        }
    }
}

/// Where an installed app lives, saved as `install.json` next to `current/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Install {
    /// The app's files are in `current/`.
    Folder,
    /// Installed system-wide from a `.deb`; `exe` is the program it ships.
    Deb { package: String, exe: PathBuf },
    /// `.app` bundle copied out of a `.dmg`.
    App { path: PathBuf },
}

impl Install {
    pub fn present(&self, current: &Path) -> bool {
        match self {
            Self::Folder => current.is_dir(),
            Self::Deb { exe, .. } => exe.exists(),
            Self::App { path } => path.is_dir(),
        }
    }

    /// Folder shown by "Show install folder".
    pub fn folder(&self, current: &Path) -> PathBuf {
        match self {
            Self::Folder => current.to_path_buf(),
            Self::Deb { exe, .. } => fs::canonicalize(exe)
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("/usr/bin")),
            Self::App { path } => path.parent().map_or_else(|| path.clone(), Path::to_path_buf),
        }
    }
}

/// Installs `file` for an app whose program is `target` (relative to its
/// install folder). `current` is the app's `current/` folder.
pub fn install(kind: Package, file: &Path, current: &Path, target: &str) -> Result<Install, String> {
    let staging = current.with_file_name("staging");
    match kind {
        Package::Zip => {
            let _ = fs::remove_dir_all(&staging);
            let archive = fs::File::open(file).map_err(|e| e.to_string())?;
            zip::ZipArchive::new(archive)
                .and_then(|mut a| a.extract_unwrapped_root_dir(&staging, zip::read::root_dir_common_filter))
                .map_err(|e| format!("Could not extract update: {e}"))?;
            replace_dir(&staging, current)?;
        }
        Package::TarGz => {
            let _ = fs::remove_dir_all(&staging);
            let archive = fs::File::open(file).map_err(|e| e.to_string())?;
            extract_tar_gz(archive, &staging).map_err(|e| format!("Could not extract update: {e}"))?;
            replace_dir(&staging, current)?;
        }
        Package::Exe => {
            let _ = fs::remove_dir_all(&staging);
            let exe = staging.join(target);
            fs::create_dir_all(exe.parent().unwrap_or(&staging)).map_err(|e| e.to_string())?;
            fs::copy(file, &exe).map_err(|e| e.to_string())?;
            replace_dir(&staging, current)?;
        }
        Package::Setup => run_setup(file, current)?,
        Package::Deb => {
            let package = install_deb(file)?;
            let exe = deb_program(&package, target)?;
            return Ok(Install::Deb { package, exe });
        }
        Package::Dmg => {
            let mount = current.with_file_name("mount");
            let path = with_dmg(file, &mount, |app| {
                let dest = applications_dir().join(app.file_name().unwrap_or(OsStr::new("App.app")));
                replace_bundle(app, &dest).map(|()| dest)
            })?;
            return Ok(Install::App { path });
        }
    }
    Ok(Install::Folder)
}

/// Removes an installed app (the caller deletes its folder afterwards).
pub fn uninstall(install: &Install, current: &Path) -> Result<(), String> {
    match install {
        Install::Folder => run_uninstaller(current),
        Install::Deb { package, .. } => privileged(&["apt-get", "remove", "-y", package.as_str()]),
        Install::App { path } => fs::remove_dir_all(path).map_err(|e| format!("{e} (is it still running?)")),
    }
}

fn replace_dir(staging: &Path, current: &Path) -> Result<(), String> {
    if current.exists() {
        fs::remove_dir_all(current).map_err(|e| format!("Could not replace old version (is it still running?): {e}"))?;
    }
    fs::rename(staging, current).map_err(|e| e.to_string())
}

/// Unpacks a `.tar.gz` into `dest`, unwrapping a single top-level folder so
/// the app's files end up directly in `dest` (like the zip path does).
pub fn extract_tar_gz(file: fs::File, dest: &Path) -> std::io::Result<()> {
    let raw = dest.with_extension("unpack");
    let _ = fs::remove_dir_all(&raw);
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(&raw)?;
    let entries: Vec<_> = fs::read_dir(&raw)?.collect::<Result<_, _>>()?;
    match entries.as_slice() {
        [only] if only.file_type()?.is_dir() => {
            fs::rename(only.path(), dest)?;
            fs::remove_dir(&raw)
        }
        _ => fs::rename(&raw, dest),
    }
}

// ---- Windows setup programs (Inno Setup) ----------------------------------

/// Runs an Inno Setup installer silently for the current user (no admin
/// prompt), installing into `dir`.
fn run_setup(file: &Path, dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new(file);
    cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/CURRENTUSER"]);
    dir_arg(&mut cmd, dir);
    let status = cmd.status().map_err(|e| format!("Could not start the installer: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("The installer failed (exit code {})", status.code().unwrap_or(-1)))
    }
}

#[cfg(windows)]
fn dir_arg(cmd: &mut Command, dir: &Path) {
    // Inno wants the quotes around the path only: /DIR="C:\a b".
    use std::os::windows::process::CommandExt;
    cmd.raw_arg(format!("/DIR=\"{}\"", dir.display()));
}

#[cfg(not(windows))]
fn dir_arg(cmd: &mut Command, dir: &Path) {
    cmd.arg(format!("/DIR={}", dir.display()));
}

/// Runs the Inno Setup uninstaller in `dir`, if there is one, and waits for
/// it to finish (it hands off to a copy of itself and exits right away).
fn run_uninstaller(dir: &Path) -> Result<(), String> {
    let Some(uninstaller) = fs::read_dir(dir).ok().into_iter().flatten().flatten().map(|e| e.path()).find(|p| {
        let name = p.file_name().and_then(OsStr::to_str).unwrap_or_default().to_lowercase();
        name.starts_with("unins") && name.ends_with(".exe")
    }) else {
        return Ok(());
    };
    Command::new(&uninstaller)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .status()
        .map_err(|e| format!("Could not start the uninstaller: {e}"))?;
    let start = Instant::now();
    while uninstaller.exists() && start.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

// ---- Linux .deb --------------------------------------------------------------

/// Installs a `.deb` with apt (falling back to dpkg) and returns its package name.
pub fn install_deb(file: &Path) -> Result<String, String> {
    let package = output("dpkg-deb", [OsStr::new("-f"), file.as_os_str(), OsStr::new("Package")])?.trim().to_string();
    let file = fs::canonicalize(file).map_err(|e| e.to_string())?;
    let file = file.to_string_lossy();
    if Path::new("/usr/bin/apt-get").exists() {
        privileged(&["apt-get", "install", "-y", "--allow-downgrades", &file])?;
    } else {
        privileged(&["dpkg", "-i", &file])?;
    }
    Ok(package)
}

/// Finds the program `target` among the files of `package`.
fn deb_program(package: &str, target: &str) -> Result<PathBuf, String> {
    let files = output("dpkg", ["-L", package])?;
    let name = Path::new(target).file_name();
    let files: Vec<PathBuf> = files.lines().map(PathBuf::from).filter(|p| p.is_file()).collect();
    let in_bin = |p: &&PathBuf| p.starts_with("/usr/bin");
    files
        .iter()
        .filter(in_bin)
        .find(|p| p.file_name() == name)
        .or_else(|| files.iter().find(|p| p.file_name() == name))
        .or_else(|| files.iter().find(in_bin))
        .cloned()
        .ok_or_else(|| format!("Package {package} has no program called {target}"))
}

/// Runs a command as root through pkexec, which shows the password dialog.
fn privileged(args: &[&str]) -> Result<(), String> {
    let status = Command::new("pkexec")
        .arg("env")
        .arg("DEBIAN_FRONTEND=noninteractive")
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|_| "Installing needs administrator rights, but pkexec is not available".to_string())?;
    match status.code() {
        Some(0) => Ok(()),
        Some(126 | 127) => Err("The administrator password was not given".into()),
        code => Err(format!("{} failed (exit code {})", args.join(" "), code.unwrap_or(-1))),
    }
}

fn output<I, S>(program: &str, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new(program).args(args).output().map_err(|e| format!("Could not run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---- macOS .dmg ------------------------------------------------------------

/// Mounts `file` at `mount`, calls `f` with the `.app` inside, then unmounts.
pub fn with_dmg<T>(file: &Path, mount: &Path, f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let detach = || {
        let _ = Command::new("hdiutil").arg("detach").arg(mount).arg("-force").output();
    };
    detach();
    fs::create_dir_all(mount).map_err(|e| e.to_string())?;
    output(
        "hdiutil",
        [
            OsStr::new("attach"),
            OsStr::new("-nobrowse"),
            OsStr::new("-readonly"),
            OsStr::new("-noautoopen"),
            OsStr::new("-mountpoint"),
            mount.as_os_str(),
            file.as_os_str(),
        ],
    )?;
    let app = fs::read_dir(mount)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension() == Some(OsStr::new("app")) && !p.is_symlink());
    let result = match app {
        Some(app) => f(&app),
        None => Err("The .dmg has no .app inside".into()),
    };
    detach();
    let _ = fs::remove_dir(mount);
    result
}

/// `/Applications` when we may write there, otherwise `~/Applications`.
fn applications_dir() -> PathBuf {
    let system = PathBuf::from("/Applications");
    let probe = system.join(".techsouls-launcher-write-test");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        return system;
    }
    let user = dirs::home_dir().unwrap_or_default().join("Applications");
    let _ = fs::create_dir_all(&user);
    user
}

/// Copies bundle `src` to `dest`, swapping out the old one only once the
/// copy is complete.
pub fn replace_bundle(src: &Path, dest: &Path) -> Result<(), String> {
    let new = dest.with_extension("app.new");
    let old = dest.with_extension("app.old");
    let _ = fs::remove_dir_all(&new);
    let _ = fs::remove_dir_all(&old);
    output("ditto", [src.as_os_str(), new.as_os_str()])?;
    if dest.exists() {
        fs::rename(dest, &old).map_err(|e| format!("Could not replace old version (is it still running?): {e}"))?;
    }
    fs::rename(&new, dest).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_packages() {
        assert_eq!(Package::of("catplay-v0.1.2-windows-x64-setup.exe"), Some(Package::Setup));
        assert_eq!(Package::of("techsouls-launcher-windows.exe"), Some(Package::Exe));
        assert_eq!(Package::of("catplay_0.1.2_amd64.deb"), Some(Package::Deb));
        assert_eq!(Package::of("CAT-macos.DMG"), Some(Package::Dmg));
        assert_eq!(Package::of("a.tgz"), Some(Package::TarGz));
        assert_eq!(Package::of("cat.msix"), None);
    }

    #[test]
    fn install_record_round_trips() {
        let deb = Install::Deb { package: "catplay".into(), exe: "/usr/bin/catplay".into() };
        let json = serde_json::to_string(&deb).unwrap();
        assert_eq!(serde_json::from_str::<Install>(&json).unwrap(), deb);
        assert_eq!(serde_json::from_str::<Install>(r#"{"kind":"folder"}"#).unwrap(), Install::Folder);
    }

    #[test]
    fn portable_exe_lands_at_target() {
        let tmp = std::env::temp_dir().join(format!("tsl-exe-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("download.exe");
        fs::write(&file, b"MZ").unwrap();
        let current = tmp.join("current");
        assert_eq!(install(Package::Exe, &file, &current, "CAT.exe").unwrap(), Install::Folder);
        assert_eq!(fs::read(current.join("CAT.exe")).unwrap(), b"MZ");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tar_gz_unwraps_root_folder() {
        let tmp = std::env::temp_dir().join(format!("tsl-tar-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let archive = tmp.join("a.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(fs::File::create(&archive).unwrap(), Default::default());
            let mut builder = tar::Builder::new(gz);
            let data = b"bin";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, "app-1.0/catplay", &data[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dest = tmp.join("staging");
        extract_tar_gz(fs::File::open(&archive).unwrap(), &dest).unwrap();
        assert_eq!(fs::read(dest.join("catplay")).unwrap(), b"bin");
        let _ = fs::remove_dir_all(&tmp);
    }
}
