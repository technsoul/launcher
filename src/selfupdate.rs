//! Keeps the launcher itself up to date from its own GitHub releases:
//! - Windows: the new `.exe` replaces the running one (renamed to `.old`)
//! - macOS: the `.app` from the `.dmg` replaces the running bundle
//! - Linux: the `.deb` is installed with apt; this asks for the password, so
//!   it waits for the user to click instead of starting on its own
//!
//! The new version starts after "Restart".

use crate::install::{self, Package};
use crate::updater::{self, Release};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::{env, fs, thread};

const REPO: &str = "technsoul/launcher";

#[derive(Debug, Clone)]
pub enum State {
    Idle,
    /// A newer release that waits for the user to start the update.
    Available(Release),
    Working,
    /// The new version is on disk; it runs after a restart.
    Ready(String),
}

enum Event {
    Found(Release),
    Applied(String),
}

pub struct SelfUpdater {
    ctx: egui::Context,
    /// The running program, read at startup (before an update renames it).
    exe: PathBuf,
    rx: Option<Receiver<Result<Option<Event>, String>>>,
    pub state: State,
}

impl SelfUpdater {
    pub fn new(ctx: egui::Context) -> Self {
        let exe = env::current_exe().unwrap_or_default();
        // Left behind by the last Windows update.
        let _ = fs::remove_file(old_exe(&exe));
        Self { ctx, exe, rx: None, state: State::Idle }
    }

    /// Looks for a newer launcher in the background and, where that needs no
    /// password, installs it right away.
    pub fn check(&mut self) {
        // Debug builds run from `target/`; never overwrite those.
        if cfg!(debug_assertions) || self.rx.is_some() || !matches!(self.state, State::Idle) {
            return;
        }
        let exe = self.exe.clone();
        self.spawn(move || {
            let release = updater::latest_release(REPO, None, &[native_package()])?;
            if !is_newer(&release.tag, env!("CARGO_PKG_VERSION")) {
                return Ok(None);
            }
            if cfg!(target_os = "linux") {
                return Ok(Some(Event::Found(release)));
            }
            apply(&exe, &release).map(|()| Some(Event::Applied(release.tag)))
        });
    }

    /// Installs an update the user agreed to.
    pub fn update(&mut self) {
        let State::Available(release) = self.state.clone() else { return };
        self.state = State::Working;
        let exe = self.exe.clone();
        self.spawn(move || apply(&exe, &release).map(|()| Some(Event::Applied(release.tag))));
    }

    fn spawn(&mut self, work: impl FnOnce() -> Result<Option<Event>, String> + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    /// Returns a message for the status line when background work finished.
    pub fn poll(&mut self) -> Option<Result<String, String>> {
        let result = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        match result {
            Ok(None) => {
                self.state = State::Idle;
                None
            }
            Ok(Some(Event::Found(release))) => {
                let msg = format!("TechSouls Launcher {} is available", release.tag);
                self.state = State::Available(release);
                Some(Ok(msg))
            }
            Ok(Some(Event::Applied(tag))) => {
                self.state = State::Ready(tag.clone());
                Some(Ok(format!("TechSouls Launcher {tag} is installed. Restart to use it")))
            }
            Err(e) => {
                self.state = State::Idle;
                Some(Err(format!("Could not update the launcher: {e}")))
            }
        }
    }

    /// Starts the updated launcher and quits this one.
    pub fn restart(&self) -> ! {
        let _ = if cfg!(target_os = "macos") {
            Command::new("open").arg("-n").arg(app_bundle(&self.exe).unwrap_or(&self.exe)).spawn()
        } else if cfg!(target_os = "linux") && Path::new(LINUX_EXE).exists() {
            Command::new(LINUX_EXE).spawn()
        } else {
            Command::new(&self.exe).spawn()
        };
        std::process::exit(0)
    }
}

/// Where the `.deb` puts the launcher.
const LINUX_EXE: &str = "/usr/bin/techsouls-launcher";

fn native_package() -> Package {
    if cfg!(windows) {
        Package::Exe
    } else if cfg!(target_os = "macos") {
        Package::Dmg
    } else {
        Package::Deb
    }
}

fn old_exe(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// `X.app` when `exe` is `X.app/Contents/MacOS/<name>`.
fn app_bundle(exe: &Path) -> Option<&Path> {
    exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app"))
}

fn apply(exe: &Path, release: &Release) -> Result<(), String> {
    let dir = env::temp_dir().join("techsouls-launcher-update");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(release.package.file_name());
    updater::download(&release.asset_url, &file, release.size, |_, _| {})?;

    let result = match release.package {
        Package::Exe => {
            // Windows lets a running program be renamed, but not overwritten.
            let old = old_exe(exe);
            let _ = fs::remove_file(&old);
            fs::rename(exe, &old).map_err(|e| format!("Could not replace {}: {e}", exe.display()))?;
            fs::copy(&file, exe).map(drop).map_err(|e| {
                let _ = fs::rename(&old, exe);
                format!("Could not write {}: {e}", exe.display())
            })
        }
        Package::Dmg => {
            let bundle = app_bundle(exe).ok_or("The launcher is not running from an .app bundle")?;
            install::with_dmg(&file, &dir.join("mount"), |app| install::replace_bundle(app, bundle))
        }
        Package::Deb => install::install_deb(&file).map(drop),
        other => Err(format!("Cannot self-update from {other:?}")),
    };
    let _ = fs::remove_dir_all(&dir);
    result
}

/// Whether release `tag` (like `v0.2.0`) is newer than `current` (`0.1.1`).
fn is_newer(tag: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let v = v.split(['-', '+']).next().unwrap_or_default();
        v.split('.').map(|n| n.parse().unwrap_or(0)).collect()
    };
    parse(tag) > parse(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("v0.1.1-beta", "0.1.1"));
    }

    #[test]
    fn finds_app_bundle() {
        let exe = Path::new("/Applications/TechSouls Launcher.app/Contents/MacOS/techsouls-launcher");
        assert_eq!(app_bundle(exe), Some(Path::new("/Applications/TechSouls Launcher.app")));
        assert_eq!(app_bundle(Path::new("/usr/bin/techsouls-launcher")), None);
    }
}
