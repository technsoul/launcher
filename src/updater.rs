//! Installs and updates apps that are published as GitHub Releases.
//!
//! Each app lives in `<data dir>/techsouls-launcher/apps/<id>/`:
//! - `current/`  the extracted release the launcher runs
//! - `version`   tag of the installed release
//!
//! Network and disk work runs on background threads; results come back
//! through a channel that the UI drains every frame with [`Updater::poll`].

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
    Failed(String),
}

enum Event {
    Checked(String, Result<Release, String>),
    Progress(String, u64, u64),
    Installed(String, Result<String, String>),
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

    /// Removes the installed release of `app` and looks it up again so it
    /// shows as installable.
    pub fn uninstall(&mut self, app: &AppEntry) -> Result<(), String> {
        if self.is_busy(&app.id) {
            return Err("it is busy downloading".into());
        }
        let root = apps_dir().join(&app.id);
        if root.exists() {
            fs::remove_dir_all(&root).map_err(|e| format!("{e} (is it still running?)"))?;
        }
        self.states.remove(&app.id);
        self.check(app);
        Ok(())
    }

    pub fn is_busy(&self, id: &str) -> bool {
        matches!(self.states.get(id), Some(State::Checking | State::Downloading { .. }))
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
            let _ = tx.send(Event::Checked(id, latest_release(&repo, pattern.as_deref())));
            ctx.request_repaint();
        });
    }

    /// Downloads and installs `release` for `app`, in the background.
    pub fn install(&mut self, app: &AppEntry, release: Release) {
        if self.is_busy(&app.id) {
            return;
        }
        self.states.insert(app.id.clone(), State::Downloading { done: 0, total: release.size });
        let (tx, ctx, id) = (self.tx.clone(), self.ctx.clone(), app.id.clone());
        thread::spawn(move || {
            let progress = |done, total| {
                let _ = tx.send(Event::Progress(id.clone(), done, total));
                ctx.request_repaint();
            };
            let result = download_and_install(&id, &release, progress).map(|()| release.tag.clone());
            let _ = tx.send(Event::Installed(id.clone(), result));
            ctx.request_repaint();
        });
    }

    /// Applies finished background work. Returns the ids of apps that were
    /// installed or updated since the last call, with an error if it failed.
    pub fn poll(&mut self) -> Vec<(String, Result<String, String>)> {
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
                Event::Installed(id, result) => {
                    let state = match &result {
                        Ok(_) => State::UpToDate,
                        Err(e) => State::Failed(e.clone()),
                    };
                    self.states.insert(id.clone(), state);
                    finished.push((id, result));
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

/// Folder the installed release of `id` runs from.
pub fn current_dir(id: &str) -> PathBuf {
    apps_dir().join(id).join("current")
}

/// Tag of the installed release, if any.
pub fn installed_version(id: &str) -> Option<String> {
    if !current_dir(id).is_dir() {
        return None;
    }
    let tag = fs::read_to_string(apps_dir().join(id).join("version")).ok()?;
    Some(tag.trim().to_string()).filter(|t| !t.is_empty())
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

fn latest_release(repo: &str, pattern: Option<&str>) -> Result<Release, String> {
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
    let asset = pick_asset(&release.assets, pattern)
        .ok_or_else(|| format!("Release {} has no .zip/.tar.gz for this platform", release.tag_name))?;
    Ok(Release {
        tag: release.tag_name,
        asset_url: asset.browser_download_url.clone(),
        size: asset.size,
        published: release.published_at.unwrap_or_default(),
    })
}

fn is_archive(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".zip") || name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

/// Picks the archive (`.zip` / `.tar.gz`) whose name contains `pattern`
/// (default: the OS name), falling back to the only archive when there is
/// exactly one.
fn pick_asset<'a>(assets: &'a [GhAsset], pattern: Option<&str>) -> Option<&'a GhAsset> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let pattern = pattern.unwrap_or(os).to_lowercase();
    let archives: Vec<_> = assets.iter().filter(|a| is_archive(&a.name)).collect();
    archives
        .iter()
        .find(|a| a.name.to_lowercase().contains(&pattern))
        .or_else(|| if archives.len() == 1 { archives.first() } else { None })
        .copied()
}

fn download_and_install(id: &str, release: &Release, progress: impl Fn(u64, u64)) -> Result<(), String> {
    let root = apps_dir().join(id);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let archive_path = root.join("download");
    let staging = root.join("staging");
    let current = root.join("current");

    download(&release.asset_url, &archive_path, release.size, progress)?;

    let _ = fs::remove_dir_all(&staging);
    let file = fs::File::open(&archive_path).map_err(|e| e.to_string())?;
    if release.asset_url.to_lowercase().ends_with(".zip") {
        zip::ZipArchive::new(file)
            .and_then(|mut archive| archive.extract_unwrapped_root_dir(&staging, zip::read::root_dir_common_filter))
            .map_err(|e| format!("Could not extract update: {e}"))?;
    } else {
        extract_tar_gz(file, &staging).map_err(|e| format!("Could not extract update: {e}"))?;
    }
    let _ = fs::remove_file(&archive_path);

    if current.exists() {
        fs::remove_dir_all(&current).map_err(|e| format!("Could not replace old version (is it still running?): {e}"))?;
    }
    fs::rename(&staging, &current).map_err(|e| e.to_string())?;
    fs::write(root.join("version"), &release.tag).map_err(|e| e.to_string())
}

/// Unpacks a `.tar.gz` into `dest`, unwrapping a single top-level folder so
/// the app's files end up directly in `dest` (like the zip path does).
fn extract_tar_gz(file: fs::File, dest: &Path) -> std::io::Result<()> {
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

fn download(url: &str, dest: &Path, expected: u64, progress: impl Fn(u64, u64)) -> Result<(), String> {
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

    fn asset(name: &str) -> GhAsset {
        GhAsset { name: name.into(), browser_download_url: format!("https://x/{name}"), size: 1 }
    }

    #[test]
    fn picks_asset_matching_pattern() {
        let assets = [asset("cat-macos.zip"), asset("CAT-Windows-x64.zip"), asset("cat.msix")];
        assert_eq!(pick_asset(&assets, Some("windows")).unwrap().name, "CAT-Windows-x64.zip");
        assert_eq!(pick_asset(&assets, Some("macos")).unwrap().name, "cat-macos.zip");
    }

    #[test]
    fn falls_back_to_single_zip() {
        let assets = [asset("cat.zip"), asset("cat.msix")];
        assert_eq!(pick_asset(&assets, Some("linux")).unwrap().name, "cat.zip");
        assert!(pick_asset(&[asset("a.zip"), asset("b.zip")], Some("linux")).is_none());
        assert!(pick_asset(&[], None).is_none());
    }

    #[test]
    fn picks_tar_gz_and_ignores_packages() {
        let assets = [
            asset("catplay-v0.1.1-linux-x64.tar.gz"),
            asset("catplay-v0.1.1-macos.zip"),
            asset("catplay-v0.1.1-windows-x64.zip"),
            asset("catplay_0.1.1_amd64.deb"),
        ];
        assert_eq!(pick_asset(&assets, Some("linux")).unwrap().name, "catplay-v0.1.1-linux-x64.tar.gz");
        assert_eq!(pick_asset(&assets, Some("windows")).unwrap().name, "catplay-v0.1.1-windows-x64.zip");
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
