//! Installs and updates apps that are published as GitHub Releases.
//!
//! Each app lives in `<data dir>/techsouls-launcher/apps/<id>/`:
//! - `current/`      the app's files (archives, portable exes, Windows setups)
//! - `install.json`  where it was installed (see [`Install`]; missing = `current/`)
//! - `version`       tag of the installed release
//!
//! Network and disk work runs on background threads; results come back
//! through a channel that the UI drains every frame with [`Updater::poll`].

use crate::install::{self, Install, Package};
use crate::manifest::AppEntry;
use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

const USER_AGENT: &str = concat!("techsouls-launcher/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub asset_url: String,
    pub package: Package,
    pub size: u64,
    /// Publish date as `YYYY-MM-DD...` (may be empty).
    pub published: String,
}

#[derive(Debug, Clone)]
pub enum State {
    Checking,
    UpToDate,
    /// A release that is not installed yet (new install or update).
    Available(Release),
    Downloading { done: u64, total: u64 },
    /// The download is done and the installer is running.
    Installing,
    Removing,
    Failed(String),
}

/// Background work that finished, reported by [`Updater::poll`].
pub enum Finished {
    /// Installed or updated, with the new tag.
    Installed(String, Result<String, String>),
    Uninstalled(String, Result<(), String>),
}

enum Event {
    Checked(String, Result<Release, String>),
    Progress(String, u64, u64),
    Installing(String),
    Done(Finished),
}

pub struct Updater {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    ctx: egui::Context,
    states: HashMap<String, State>,
    /// Latest release found for each app by the last successful check.
    latest: HashMap<String, Release>,
}

impl Updater {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, ctx, states: HashMap::new(), latest: HashMap::new() }
    }

    pub fn state(&self, id: &str) -> Option<&State> {
        self.states.get(id)
    }

    pub fn latest(&self, id: &str) -> Option<&Release> {
        self.latest.get(id)
    }

    /// Removes the installed app in the background (running its uninstaller
    /// or package manager when it has one).
    pub fn uninstall(&mut self, app: &AppEntry) -> Result<(), String> {
        if self.is_busy(&app.id) {
            return Err("it is busy".into());
        }
        self.states.insert(app.id.clone(), State::Removing);
        let (tx, ctx, id) = (self.tx.clone(), self.ctx.clone(), app.id.clone());
        thread::spawn(move || {
            let result = remove(&id);
            let _ = tx.send(Event::Done(Finished::Uninstalled(id, result)));
            ctx.request_repaint();
        });
        Ok(())
    }

    pub fn is_busy(&self, id: &str) -> bool {
        matches!(
            self.states.get(id),
            Some(State::Checking | State::Downloading { .. } | State::Installing | State::Removing)
        )
    }

    /// Asks GitHub for the latest release of `app`, in the background.
    pub fn check(&mut self, app: &AppEntry) {
        let Some(repo) = app.github.clone() else { return };
        if self.is_busy(&app.id) {
            return;
        }
        self.states.insert(app.id.clone(), State::Checking);
        let (tx, ctx, id, pattern) = (self.tx.clone(), self.ctx.clone(), app.id.clone(), app.asset.clone());
        thread::spawn(move || {
            let _ = tx.send(Event::Checked(id, latest_release(&repo, pattern.as_deref(), Package::preferred())));
            ctx.request_repaint();
        });
    }

    /// Downloads and installs `release` for `app`, in the background.
    pub fn install(&mut self, app: &AppEntry, release: Release) {
        if self.is_busy(&app.id) {
            return;
        }
        self.states.insert(app.id.clone(), State::Downloading { done: 0, total: release.size });
        let (tx, ctx, id, target) = (self.tx.clone(), self.ctx.clone(), app.id.clone(), app.target.clone());
        thread::spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            let result = download_and_install(
                &id,
                &target,
                &release,
                |done, total| send(Event::Progress(id.clone(), done, total)),
                || send(Event::Installing(id.clone())),
            )
            .map(|()| release.tag.clone());
            send(Event::Done(Finished::Installed(id.clone(), result)));
        });
    }

    /// Applies finished background work and returns what finished since the
    /// last call.
    pub fn poll(&mut self) -> Vec<Finished> {
        let mut finished = vec![];
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Checked(id, Ok(release)) => {
                    let state = if installed_version(&id).as_deref() == Some(release.tag.as_str()) {
                        State::UpToDate
                    } else {
                        State::Available(release.clone())
                    };
                    self.latest.insert(id.clone(), release);
                    self.states.insert(id, state);
                }
                Event::Checked(id, Err(e)) => {
                    self.states.insert(id, State::Failed(e));
                }
                Event::Progress(id, done, total) => {
                    self.states.insert(id, State::Downloading { done, total });
                }
                Event::Installing(id) => {
                    self.states.insert(id, State::Installing);
                }
                Event::Done(done) => {
                    match &done {
                        Finished::Installed(id, Ok(_)) => {
                            self.states.insert(id.clone(), State::UpToDate);
                        }
                        Finished::Uninstalled(id, Ok(())) => {
                            self.states.remove(id);
                        }
                        Finished::Installed(id, Err(e)) | Finished::Uninstalled(id, Err(e)) => {
                            self.states.insert(id.clone(), State::Failed(e.clone()));
                        }
                    }
                    finished.push(done);
                }
            }
        }
        finished
    }
}

fn apps_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("techsouls-launcher")
        .join("apps")
}

/// Folder that holds the app's files, when they are not installed elsewhere.
pub fn current_dir(id: &str) -> PathBuf {
    apps_dir().join(id).join("current")
}

/// How `id` is installed, if it is.
pub fn installation(id: &str) -> Option<Install> {
    let install = fs::read_to_string(apps_dir().join(id).join("install.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Install::Folder);
    install.present(&current_dir(id)).then_some(install)
}

/// Folder to show for an installed app.
pub fn install_folder(id: &str) -> PathBuf {
    installation(id).map_or_else(|| current_dir(id), |i| i.folder(&current_dir(id)))
}

/// Tag of the installed release, if any.
pub fn installed_version(id: &str) -> Option<String> {
    installation(id)?;
    let tag = fs::read_to_string(apps_dir().join(id).join("version")).ok()?;
    Some(tag.trim().to_string()).filter(|t| !t.is_empty())
}

fn remove(id: &str) -> Result<(), String> {
    let root = apps_dir().join(id);
    if let Some(installed) = installation(id) {
        install::uninstall(&installed, &current_dir(id))?;
    }
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|e| format!("{e} (is it still running?)"))?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Latest release of `repo` with a file of one of the `accepted` kinds.
pub fn latest_release(repo: &str, pattern: Option<&str>, accepted: &[Package]) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut response = ureq::get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => format!("{repo} has no published release"),
            e => format!("Update check failed: {e}"),
        })?;
    let release: GhRelease = response.body_mut().read_json().map_err(|e| e.to_string())?;
    let (asset, package) = pick_asset(&release.assets, pattern, os_name(), accepted)
        .ok_or_else(|| format!("Release {} has no download for {}", release.tag_name, os_name()))?;
    Ok(Release {
        tag: release.tag_name,
        asset_url: asset.browser_download_url.clone(),
        package,
        size: asset.size,
        published: release.published_at.unwrap_or_default(),
    })
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Picks the best file for this OS, trying the `accepted` kinds in order.
/// With a `pattern` the name must contain it. Without one, an archive must
/// name the OS (or be the only archive), since archives fit any OS.
fn pick_asset<'a>(
    assets: &'a [GhAsset],
    pattern: Option<&str>,
    os: &str,
    accepted: &[Package],
) -> Option<(&'a GhAsset, Package)> {
    let named = |a: &GhAsset, part: &str| a.name.to_lowercase().contains(&part.to_lowercase());
    let archives = assets.iter().filter(|a| Package::of(&a.name).is_some_and(Package::is_archive)).count();
    accepted.iter().find_map(|&kind| {
        let mut of_kind = assets.iter().filter(|a| Package::of(&a.name) == Some(kind));
        let asset = match pattern {
            Some(p) => of_kind.find(|a| named(a, p)),
            None if kind.is_archive() => {
                let all: Vec<_> = of_kind.collect();
                all.iter().find(|a| named(a, os)).or(if archives == 1 { all.first() } else { None }).copied()
            }
            None => of_kind.next(),
        };
        asset.map(|a| (a, kind))
    })
}

fn download_and_install(
    id: &str,
    target: &str,
    release: &Release,
    progress: impl Fn(u64, u64),
    installing: impl Fn(),
) -> Result<(), String> {
    let root = apps_dir().join(id);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let file = root.join(release.package.file_name());
    download(&release.asset_url, &file, release.size, progress)?;

    installing();
    let current = root.join("current");
    let result = install::install(release.package, &file, &current, target);
    let _ = fs::remove_file(&file);
    let installed = result?;
    if installed != Install::Folder {
        let _ = fs::remove_dir_all(&current);
    }
    let record = serde_json::to_string_pretty(&installed).map_err(|e| e.to_string())?;
    fs::write(root.join("install.json"), record).map_err(|e| e.to_string())?;
    fs::write(root.join("version"), &release.tag).map_err(|e| e.to_string())
}

pub fn download(url: &str, dest: &Path, expected: u64, progress: impl Fn(u64, u64)) -> Result<(), String> {
    let response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let mut reader = response.into_body().into_reader();
    let mut out = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0; 64 * 1024];
    let (mut done, mut reported) = (0u64, 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if done - reported >= 512 * 1024 {
            progress(done, expected);
            reported = done;
        }
    }
    progress(done, expected);
    if expected > 0 && done != expected {
        return Err(format!("Download incomplete ({done} of {expected} bytes)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Package::*;

    fn asset(name: &str) -> GhAsset {
        GhAsset { name: name.into(), browser_download_url: format!("https://x/{name}"), size: 1 }
    }

    fn pick<'a>(assets: &'a [GhAsset], pattern: Option<&str>, os: &str, accepted: &[Package]) -> Option<&'a str> {
        pick_asset(assets, pattern, os, accepted).map(|(a, _)| a.name.as_str())
    }

    const CATPLAY: [&str; 3] =
        ["catplay-v0.1.2-windows-x64-setup.exe", "catplay_0.1.2_amd64.deb", "catplay-v0.1.2-macos.dmg"];

    #[test]
    fn picks_native_package_per_os() {
        let assets = CATPLAY.map(asset);
        assert_eq!(pick(&assets, None, "windows", &[Setup, Exe, Zip]), Some(CATPLAY[0]));
        assert_eq!(pick(&assets, None, "linux", &[Deb, TarGz, Zip]), Some(CATPLAY[1]));
        assert_eq!(pick(&assets, None, "macos", &[Dmg, Zip, TarGz]), Some(CATPLAY[2]));
        // A Linux without dpkg can't use any of these.
        assert_eq!(pick(&assets, None, "linux", &[TarGz, Zip]), None);
    }

    #[test]
    fn prefers_native_package_over_archive() {
        let assets = [asset("cat-windows.zip"), asset("cat-windows-setup.exe")];
        assert_eq!(pick(&assets, None, "windows", &[Setup, Exe, Zip]), Some("cat-windows-setup.exe"));
    }

    #[test]
    fn archives_must_name_the_os_unless_only_one() {
        let assets = [asset("cat-macos.zip"), asset("CAT-Windows-x64.zip"), asset("cat.msix")];
        assert_eq!(pick(&assets, None, "windows", &[Setup, Exe, Zip]), Some("CAT-Windows-x64.zip"));
        assert_eq!(pick(&assets, None, "linux", &[Deb, TarGz, Zip]), None);
        assert_eq!(pick(&[asset("cat.zip")], None, "linux", &[TarGz, Zip]), Some("cat.zip"));
        assert_eq!(pick(&[], None, "linux", &[TarGz, Zip]), None);
    }

    #[test]
    fn pattern_overrides_os_name() {
        let assets = [asset("cat-x64.zip"), asset("cat-arm64.zip")];
        assert_eq!(pick(&assets, Some("ARM64"), "linux", &[TarGz, Zip]), Some("cat-arm64.zip"));
    }
}
