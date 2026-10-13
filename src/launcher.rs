use crate::manifest::{self, AppEntry, LaunchKind, Manifest};
use crate::install::Install;
use crate::selfupdate::{self, SelfUpdater};
use crate::updater::{self, Finished, Release, State, Updater};
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Galley, Id, Painter, Pos2, Rect,
    ResizeDirection, Response, Sense, Stroke, StrokeKind, Vec2, ViewportCommand,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often apps are checked for new releases.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const CARD_MIN_WIDTH: f32 = 330.0;
const CARD_HEIGHT: f32 = 272.0;
const ROW_HEIGHT: f32 = 68.0;
const TITLE_HEIGHT: f32 = 48.0;
const COMPANY_SITE: &str = "https://www.techsouls.tech";
const COMPANY_GITHUB: &str = "https://github.com/technsoul";

const BG: Color32 = Color32::from_rgb(17, 18, 20);
const BAR: Color32 = Color32::from_rgb(12, 13, 14);
const HOVER: Color32 = Color32::from_rgb(25, 26, 29);
const LINE: Color32 = Color32::from_rgb(42, 43, 46);
const TEXT: Color32 = Color32::from_rgb(240, 240, 240);
const SOFT: Color32 = Color32::from_rgb(178, 179, 183);
const MUTED: Color32 = Color32::from_rgb(128, 129, 134);
const FAINT: Color32 = Color32::from_rgb(84, 85, 90);
const BRAND: Color32 = Color32::from_rgb(30, 123, 242);
const GREEN: Color32 = Color32::from_rgb(74, 222, 128);
const BLUE: Color32 = Color32::from_rgb(96, 165, 250);
const AMBER: Color32 = Color32::from_rgb(251, 191, 36);
const RED: Color32 = Color32::from_rgb(248, 113, 113);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Installed,
    Updates,
    NotInstalled,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Installed, Filter::Updates, Filter::NotInstalled];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "ALL",
            Filter::Installed => "INSTALLED",
            Filter::Updates => "UPDATES",
            Filter::NotInstalled => "NOT INSTALLED",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Grid,
    Table,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Suite,
    Name,
    Status,
}

impl Sort {
    const ALL: [Sort; 3] = [Sort::Suite, Sort::Name, Sort::Status];

    fn label(self) -> &'static str {
        match self {
            Sort::Suite => "Suite order",
            Sort::Name => "Name",
            Sort::Status => "Status",
        }
    }
}

/// What an app can do right now.
#[derive(Clone)]
enum Phase {
    /// Can be opened.
    Ready,
    Update(Release),
    Install(Release),
    Checking,
    Downloading(Option<f32>),
    /// Installing or removing; the label says which.
    Busy(&'static str),
    Unavailable(String),
}

/// One app plus everything the UI shows about it, computed once per frame.
struct Row {
    number: usize,
    app: AppEntry,
    phase: Phase,
    installed: bool,
    up_to_date: bool,
    version: Option<String>,
    date: Option<String>,
    running: usize,
}

impl Row {
    fn accent(&self) -> Color32 {
        self.app.color.as_deref().and_then(parse_hex).unwrap_or(BRAND)
    }

    fn matches(&self, filter: Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Installed => self.installed,
            Filter::Updates => matches!(self.phase, Phase::Update(_)),
            Filter::NotInstalled => !self.installed,
        }
    }

    fn rank(&self) -> u8 {
        match self.phase {
            Phase::Update(_) => 0,
            Phase::Ready => 1,
            Phase::Downloading(_) | Phase::Busy(_) => 2,
            Phase::Install(_) => 3,
            Phase::Checking => 4,
            Phase::Unavailable(_) => 5,
        }
    }

    fn chip(&self) -> (String, Color32) {
        let (text, color) = match &self.phase {
            Phase::Ready if self.app.github.is_none() => match self.app.kind {
                LaunchKind::Url => ("WEB APP".to_string(), SOFT),
                LaunchKind::Exe => ("LOCAL APP".to_string(), GREEN),
            },
            Phase::Ready if self.up_to_date => ("INSTALLED · UP TO DATE".to_string(), GREEN),
            Phase::Ready => ("INSTALLED".to_string(), GREEN),
            Phase::Update(r) => (format!("UPDATE AVAILABLE · {}", r.tag), AMBER),
            Phase::Install(_) => ("AVAILABLE TO INSTALL".to_string(), BLUE),
            Phase::Checking => ("CHECKING".to_string(), MUTED),
            Phase::Downloading(_) => ("DOWNLOADING".to_string(), BLUE),
            Phase::Busy(label) => (label.to_string(), BLUE),
            Phase::Unavailable(_) => ("UNAVAILABLE".to_string(), RED),
        };
        if self.running > 0 { (format!("{text} · RUNNING"), color) } else { (text, color) }
    }

    fn version_line(&self) -> String {
        match (&self.version, &self.date) {
            (Some(v), Some(d)) => format!("Version {} · {d}", v.trim_start_matches('v')),
            (Some(v), None) => format!("Version {}", v.trim_start_matches('v')),
            (None, _) if self.app.github.is_none() => match self.app.kind {
                LaunchKind::Url => "Opens in your browser".into(),
                LaunchKind::Exe => "Runs from this computer".into(),
            },
            (None, _) if matches!(self.phase, Phase::Unavailable(_)) => "No release published yet".into(),
            (None, _) => "Looking for the latest release…".into(),
        }
    }

    /// Label, filled style, enabled, progress.
    fn primary(&self) -> (String, bool, bool, Option<f32>) {
        match &self.phase {
            Phase::Ready => ("OPEN".into(), false, true, None),
            Phase::Update(_) => ("UPDATE".into(), true, self.running == 0, None),
            Phase::Install(_) => ("INSTALL".into(), true, true, None),
            Phase::Checking => ("CHECKING…".into(), false, false, None),
            Phase::Downloading(Some(p)) => (format!("DOWNLOADING {}%", (p * 100.0) as u32), false, false, Some(*p)),
            Phase::Downloading(None) => ("DOWNLOADING…".into(), false, false, None),
            Phase::Busy(label) => (format!("{label}…"), false, false, None),
            Phase::Unavailable(_) => ("RETRY".into(), false, true, None),
        }
    }

    fn primary_cmd(&self) -> Option<Cmd> {
        let app = self.app.clone();
        match &self.phase {
            Phase::Ready => Some(Cmd::Open(app)),
            Phase::Update(r) | Phase::Install(r) => Some(Cmd::Install(app, r.clone())),
            Phase::Unavailable(_) => Some(Cmd::Check(app)),
            Phase::Checking | Phase::Downloading(_) | Phase::Busy(_) => None,
        }
    }

    fn error(&self) -> Option<&str> {
        match &self.phase {
            Phase::Unavailable(e) => Some(e),
            _ => None,
        }
    }
}

/// User actions collected while drawing, applied after the frame's UI.
enum Cmd {
    Open(AppEntry),
    Install(AppEntry, Release),
    Check(AppEntry),
    Uninstall(AppEntry),
    Reveal(PathBuf),
    Url(String),
    Reload,
    UpdateLauncher,
    RestartLauncher,
}

pub struct Launcher {
    manifest: Manifest,
    load_error: Option<String>,
    search: String,
    filter: Filter,
    view: View,
    sort: Sort,
    /// Processes we spawned, keyed by app id.
    running: HashMap<String, Vec<Child>>,
    status: Option<(String, bool)>,
    updater: Updater,
    selfupdate: SelfUpdater,
    last_check: Instant,
    /// Apps to launch as soon as their install/update finishes.
    launch_after_install: HashSet<String>,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        install_fonts(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let mut this = Self {
            manifest: Manifest { apps: vec![] },
            load_error: None,
            search: String::new(),
            filter: Filter::All,
            view: View::Grid,
            sort: Sort::Suite,
            running: HashMap::new(),
            status: None,
            updater: Updater::new(cc.egui_ctx.clone()),
            selfupdate: SelfUpdater::new(cc.egui_ctx.clone()),
            last_check: Instant::now(),
            launch_after_install: HashSet::new(),
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        match manifest::load() {
            Ok(m) => {
                self.manifest = m;
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(e),
        }
        self.check_updates();
    }

    fn check_updates(&mut self) {
        self.last_check = Instant::now();
        self.selfupdate.check();
        for app in &self.manifest.apps {
            self.updater.check(app);
        }
    }

    /// Handles finished installs, starts updates for apps that are idle and
    /// launches apps the user clicked once they are ready.
    fn drive_updates(&mut self) {
        if let Some(result) = self.selfupdate.poll() {
            self.status = Some(match result {
                Ok(msg) => (msg, false),
                Err(e) => (e, true),
            });
        }
        for finished in self.updater.poll() {
            let id = match &finished {
                Finished::Installed(id, _) | Finished::Uninstalled(id, _) => id.clone(),
            };
            let Some(app) = self.manifest.apps.iter().find(|a| a.id == id).cloned() else { continue };
            self.status = Some(match finished {
                Finished::Installed(_, Ok(tag)) => (format!("{} updated to {tag}", app.name), false),
                Finished::Installed(_, Err(e)) => {
                    self.launch_after_install.remove(&id);
                    (format!("Could not update {}: {e}", app.name), true)
                }
                Finished::Uninstalled(_, Ok(())) => {
                    self.updater.check(&app);
                    (format!("{} was uninstalled", app.name), false)
                }
                Finished::Uninstalled(_, Err(e)) => (format!("Could not uninstall {}: {e}", app.name), true),
            });
        }

        // Update installed apps automatically, but never while they run.
        let idle_updates: Vec<_> = self
            .manifest
            .apps
            .iter()
            .filter(|a| updater::installed_version(&a.id).is_some() && self.running_count(&a.id) == 0)
            .filter_map(|a| match self.updater.state(&a.id) {
                Some(State::Available(release)) => Some((a.clone(), release.clone())),
                _ => None,
            })
            .collect();
        for (app, release) in idle_updates {
            self.updater.install(&app, release);
        }

        // Retry clicked apps whose check or install has settled.
        let pending: Vec<_> = self
            .manifest
            .apps
            .iter()
            .filter(|a| self.launch_after_install.contains(&a.id) && !self.updater.is_busy(&a.id))
            .cloned()
            .collect();
        for app in pending {
            self.launch_after_install.remove(&app.id);
            self.launch(&app);
        }

        if self.last_check.elapsed() >= CHECK_INTERVAL {
            self.check_updates();
        }
    }

    fn row(&self, number: usize, app: &AppEntry) -> Row {
        let installed_tag = app.github.as_ref().and_then(|_| updater::installed_version(&app.id));
        let installed = app.github.is_none() || installed_tag.is_some();
        let state = self.updater.state(&app.id);
        let phase = match state {
            _ if app.github.is_none() => Phase::Ready,
            Some(State::Downloading { done, total }) => {
                Phase::Downloading((*total > 0).then(|| *done as f32 / *total as f32))
            }
            Some(State::Installing) => Phase::Busy("INSTALLING"),
            Some(State::Removing) => Phase::Busy("REMOVING"),
            Some(State::Available(r)) if installed => Phase::Update(r.clone()),
            Some(State::Available(r)) => Phase::Install(r.clone()),
            _ if installed => Phase::Ready,
            Some(State::Failed(e)) => Phase::Unavailable(e.clone()),
            _ => Phase::Checking,
        };
        let latest = self.updater.latest(&app.id);
        let version = installed_tag.or_else(|| latest.map(|r| r.tag.clone()));
        let date = latest
            .filter(|r| Some(&r.tag) == version.as_ref())
            .and_then(|r| format_date(&r.published));
        Row {
            number,
            app: app.clone(),
            phase,
            installed,
            up_to_date: matches!(state, Some(State::UpToDate)),
            version,
            date,
            running: self.running_count(&app.id),
        }
    }

    fn visible<'a>(&self, rows: &'a [Row]) -> Vec<&'a Row> {
        let q = self.search.trim().to_lowercase();
        let mut visible: Vec<&Row> = rows
            .iter()
            .filter(|r| r.matches(self.filter))
            .filter(|r| {
                q.is_empty()
                    || r.app.name.to_lowercase().contains(&q)
                    || r.app.description.to_lowercase().contains(&q)
                    || r.app.category.to_lowercase().contains(&q)
            })
            .collect();
        match self.sort {
            Sort::Suite => {}
            Sort::Name => visible.sort_by_key(|r| r.app.name.to_lowercase()),
            Sort::Status => visible.sort_by_key(|r| r.rank()),
        }
        visible
    }

    fn launch(&mut self, app: &AppEntry) {
        if app.github.is_some() && !self.ready_to_launch(app) {
            return;
        }
        let result = match app.kind {
            LaunchKind::Url => open::that_detached(&app.target).map_err(|e| e.to_string()),
            LaunchKind::Exe => exe_command(app)
                .args(&app.args)
                .spawn()
                .map(|child| self.running.entry(app.id.clone()).or_default().push(child))
                .map_err(|e| e.to_string()),
        };
        self.status = Some(match result {
            Ok(()) => (format!("Launched {}", app.name), false),
            Err(e) => (format!("Failed to launch {}: {e}", app.name), true),
        });
    }

    /// For GitHub apps: installs or finishes updating first, and queues the
    /// launch for afterwards. Returns whether it can start right now.
    fn ready_to_launch(&mut self, app: &AppEntry) -> bool {
        let installed = updater::installed_version(&app.id).is_some();
        let wait = |this: &mut Self, msg: String| {
            this.launch_after_install.insert(app.id.clone());
            this.status = Some((msg, false));
            false
        };
        match self.updater.state(&app.id).cloned() {
            Some(State::Downloading { .. } | State::Installing) => {
                wait(self, format!("{} will start when the install finishes", app.name))
            }
            Some(State::Available(release)) if !installed || self.running_count(&app.id) == 0 => {
                self.updater.install(app, release);
                wait(self, format!("Downloading {}…", app.name))
            }
            _ if installed => true,
            Some(State::Checking) => wait(self, format!("Looking for {}…", app.name)),
            Some(State::Failed(e)) => {
                self.status = Some((format!("{} is not installed: {e}", app.name), true));
                self.updater.check(app);
                false
            }
            _ => {
                self.updater.check(app);
                wait(self, format!("Looking for {}…", app.name))
            }
        }
    }

    fn run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Open(app) => self.launch(&app),
            Cmd::Install(app, release) => {
                self.status = Some((format!("Downloading {}…", app.name), false));
                self.updater.install(&app, release);
            }
            Cmd::Check(app) => self.updater.check(&app),
            Cmd::Uninstall(app) => {
                self.status = Some(match self.updater.uninstall(&app) {
                    Ok(()) => (format!("Removing {}…", app.name), false),
                    Err(e) => (format!("Could not uninstall {}: {e}", app.name), true),
                });
            }
            Cmd::Reveal(path) => {
                let _ = open::that_detached(path);
            }
            Cmd::Url(url) => {
                let _ = open::that_detached(url);
            }
            Cmd::Reload => self.reload(),
            Cmd::UpdateLauncher => {
                self.status = Some(("Updating TechSouls Launcher…".into(), false));
                self.selfupdate.update();
            }
            Cmd::RestartLauncher => self.selfupdate.restart(),
        }
    }

    /// Drops processes that have exited.
    fn reap(&mut self) {
        for children in self.running.values_mut() {
            children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        }
        self.running.retain(|_, c| !c.is_empty());
    }

    fn running_count(&self, id: &str) -> usize {
        self.running.get(id).map_or(0, Vec::len)
    }

    fn title_bar(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        egui::Panel::top("title")
            .exact_size(TITLE_HEIGHT)
            .frame(egui::Frame::new().fill(BAR))
            .show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let bar = ui.max_rect();
                let drag = ui.interact(bar, Id::new("title-drag"), Sense::click_and_drag());
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                if drag.drag_started() {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }

                let p = ui.painter().clone();
                let cy = bar.center().y;

                // Wordmark: TECH in white, SOULS in brand blue.
                let mut job = LayoutJob::default();
                for (text, color) in [("TECH", TEXT), ("SOULS", BRAND)] {
                    job.append(
                        text,
                        0.0,
                        TextFormat { font_id: bold(20.0), color, extra_letter_spacing: 1.5, ..Default::default() },
                    );
                }
                let logo = p.layout_job(job);
                let mut x = bar.left() + 20.0;
                p.galley(Pos2::new(x, cy - logo.size().y / 2.0), logo.clone(), TEXT);
                x += logo.size().x + 22.0;
                p.vline(x, bar.y_range(), Stroke::new(1.0, LINE));
                x += 16.0;
                let tag = galley(ui, "LAUNCHER", mono(11.0), Color32::PLACEHOLDER, 1.5);
                let tag_rect = Rect::from_min_size(Pos2::new(x, cy - 11.0), Vec2::new(tag.size().x + 16.0, 22.0));
                p.rect_filled(tag_rect, 0, TEXT);
                p.galley(tag_rect.center() - tag.size() / 2.0, tag, BAR);
                let suite = galley(ui, "APP SUITE", mono(11.0), Color32::PLACEHOLDER, 2.0);
                p.galley(Pos2::new(tag_rect.right() + 12.0, cy - suite.size().y / 2.0), suite, MUTED);

                // Right side, laid out from the right edge.
                let mut right = bar.right();
                let slot = |right: &mut f32, w: f32| {
                    *right -= w;
                    Rect::from_min_size(Pos2::new(*right, bar.top()), Vec2::new(w, bar.height()))
                };

                let close = slot(&mut right, 46.0);
                let r = hit(ui, close, Id::new("win-close"));
                if r.hovered() {
                    p.rect_filled(close, 0, Color32::from_rgb(196, 43, 28));
                }
                let c = close.center();
                let s = Stroke::new(1.2, TEXT);
                p.line_segment([c + Vec2::splat(-5.0), c + Vec2::splat(5.0)], s);
                p.line_segment([c + Vec2::new(-5.0, 5.0), c + Vec2::new(5.0, -5.0)], s);
                if r.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }

                let max = slot(&mut right, 46.0);
                let r = hit(ui, max, Id::new("win-max"));
                hover_fill(&p, &r, max);
                let c = max.center();
                if maximized {
                    p.rect_stroke(Rect::from_center_size(c + Vec2::new(-1.5, 1.5), Vec2::splat(8.0)), 0, s, StrokeKind::Middle);
                    p.line_segment([c + Vec2::new(-2.5, -5.5), c + Vec2::new(5.5, -5.5)], s);
                    p.line_segment([c + Vec2::new(5.5, -5.5), c + Vec2::new(5.5, 2.5)], s);
                } else {
                    p.rect_stroke(Rect::from_center_size(c, Vec2::splat(10.0)), 0, s, StrokeKind::Middle);
                }
                if r.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }

                let min = slot(&mut right, 46.0);
                let r = hit(ui, min, Id::new("win-min"));
                hover_fill(&p, &r, min);
                p.hline(min.center().x - 5.0..=min.center().x + 5.0, min.center().y, s);
                if r.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }

                p.vline(right, bar.y_range(), Stroke::new(1.0, LINE));

                let settings = slot(&mut right, 46.0);
                let r = hit(ui, settings, Id::new("settings")).on_hover_text(format!(
                    "Open the config folder\n{}",
                    manifest::manifest_path().display()
                ));
                hover_fill(&p, &r, settings);
                p.text(settings.center(), Align2::CENTER_CENTER, "⚙", FontId::proportional(17.0), SOFT);
                if r.clicked() {
                    cmds.push(Cmd::Reveal(manifest::config_dir()));
                }

                let refresh = slot(&mut right, 46.0);
                let r = hit(ui, refresh, Id::new("refresh")).on_hover_text("Reload apps and check for updates");
                hover_fill(&p, &r, refresh);
                p.text(refresh.center(), Align2::CENTER_CENTER, "⟳", FontId::proportional(18.0), SOFT);
                if r.clicked() {
                    cmds.push(Cmd::Reload);
                }

                p.vline(right, bar.y_range(), Stroke::new(1.0, LINE));

                let site = galley(ui, "WEBSITE", mono(11.0), Color32::PLACEHOLDER, 2.0);
                let site_rect = slot(&mut right, site.size().x + 40.0);
                let r = hit(ui, site_rect, Id::new("company-site")).on_hover_text(COMPANY_SITE);
                hover_fill(&p, &r, site_rect);
                p.galley(site_rect.center() - site.size() / 2.0, site, if r.hovered() { TEXT } else { SOFT });
                if r.clicked() {
                    cmds.push(Cmd::Url(COMPANY_SITE.into()));
                }

                p.vline(right, bar.y_range(), Stroke::new(1.0, LINE));
                right -= 12.0;

                for (view, label) in [(View::Table, "TABLE"), (View::Grid, "GRID")] {
                    let g = galley(ui, label, mono(11.0), Color32::PLACEHOLDER, 2.0);
                    let w = g.size().x + 16.0;
                    right -= w + 4.0;
                    let rect = Rect::from_min_size(Pos2::new(right, cy - 11.0), Vec2::new(w, 22.0));
                    let r = hit(ui, rect, Id::new(("view", label)));
                    let selected = self.view == view;
                    let fg = if selected {
                        p.rect_filled(rect, 0, TEXT);
                        BAR
                    } else if r.hovered() {
                        TEXT
                    } else {
                        SOFT
                    };
                    p.galley(rect.center() - g.size() / 2.0, g, fg);
                    if r.clicked() {
                        self.view = view;
                    }
                }
            });
    }

    fn footer(&self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        egui::Panel::bottom("footer")
            .exact_size(30.0)
            .frame(egui::Frame::new().fill(BAR))
            .show(ui, |ui| {
                let bar = ui.max_rect();
                let p = ui.painter().clone();
                let cy = bar.center().y;

                let platform = format!("INSTALLING FOR {}", platform_name());
                let g = galley(ui, platform, mono(10.0), Color32::PLACEHOLDER, 2.0);
                let mut right = bar.right() - 20.0 - g.size().x;
                p.galley(Pos2::new(right, cy - g.size().y / 2.0), g, FAINT);
                right -= 28.0;
                right = link(ui, Pos2::new(right, cy), "GITHUB", Id::new("footer-gh"), COMPANY_GITHUB, cmds) - 18.0;
                let mut links_left = link(ui, Pos2::new(right, cy), "TECHSOULS.TECH", Id::new("footer-site"), COMPANY_SITE, cmds);

                // Launcher self-update: needs a click to start (Linux) or to restart.
                let action = match &self.selfupdate.state {
                    selfupdate::State::Available(r) => Some((format!("UPDATE LAUNCHER TO {}", r.tag), Cmd::UpdateLauncher)),
                    selfupdate::State::Ready(tag) => Some((format!("RESTART TO USE {tag}"), Cmd::RestartLauncher)),
                    _ => None,
                };
                if let Some((label, cmd)) = action {
                    let g = galley(ui, label, mono(10.5), Color32::PLACEHOLDER, 1.8);
                    let size = g.size() + Vec2::new(16.0, 8.0);
                    let rect = Rect::from_min_size(Pos2::new(links_left - 28.0 - size.x, cy - size.y / 2.0), size);
                    let r = hit(ui, rect, Id::new("footer-selfupdate"));
                    p.rect_filled(rect, 0, if r.hovered() { TEXT } else { BRAND });
                    p.galley(rect.center() - g.size() / 2.0, g, BAR);
                    if r.clicked() {
                        cmds.push(cmd);
                    }
                    links_left = rect.left();
                }

                let max_width = (links_left - bar.left() - 60.0).max(40.0);
                let (text, font, color, spacing) = match &self.status {
                    Some((msg, true)) => (msg.clone(), FontId::proportional(12.5), RED, 0.0),
                    Some((msg, false)) => (msg.clone(), FontId::proportional(12.5), SOFT, 0.0),
                    None => (
                        format!("CHECKED {} · USING GITHUB RELEASES", ago(self.last_check.elapsed())),
                        mono(10.0),
                        FAINT,
                        2.0,
                    ),
                };
                let g = text_job(ui, text, font, Color32::PLACEHOLDER, spacing, max_width);
                p.galley(Pos2::new(bar.left() + 20.0, cy - g.size().y / 2.0), g, color);
            });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, rows: &[Row]) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::hover());
        let p = ui.painter().clone();

        // Filter tabs.
        let mut x = rect.left();
        for (i, filter) in Filter::ALL.into_iter().enumerate() {
            let count = rows.iter().filter(|r| r.matches(filter)).count();
            let label = galley(ui, filter.label(), mono(11.0), Color32::PLACEHOLDER, 1.5);
            let num = galley(ui, count.to_string(), mono(10.0), Color32::PLACEHOLDER, 1.0);
            let w = label.size().x + num.size().x + 32.0;
            let tab = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w, rect.height()));
            let r = hit(ui, tab, Id::new(("filter", i)));
            let selected = self.filter == filter;
            let (fg, num_fg) = if selected {
                p.rect_filled(tab, 0, TEXT);
                (BG, MUTED)
            } else {
                hover_fill(&p, &r, tab);
                (if r.hovered() { TEXT } else { SOFT }, FAINT)
            };
            let lx = tab.left() + 12.0;
            p.galley(Pos2::new(lx, tab.center().y - label.size().y / 2.0), label.clone(), fg);
            p.galley(
                Pos2::new(lx + label.size().x + 8.0, tab.center().y - num.size().y / 2.0),
                num,
                num_fg,
            );
            if i > 0 {
                p.vline(tab.left(), tab.y_range(), Stroke::new(1.0, LINE));
            }
            if r.clicked() {
                self.filter = filter;
            }
            x += w;
        }
        p.rect_stroke(
            Rect::from_x_y_ranges(rect.left()..=x, rect.y_range()),
            0,
            Stroke::new(1.0, LINE),
            StrokeKind::Inside,
        );

        // Search box.
        let search_w = 220.0_f32.min(rect.width() * 0.25);
        let search_rect = Rect::from_min_size(Pos2::new(rect.right() - search_w, rect.top()), Vec2::new(search_w, rect.height()));
        ui.put(
            search_rect,
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search apps")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(10, 8)),
        );

        // Sort dropdown.
        let sort_rect = Rect::from_min_size(
            Pos2::new(search_rect.left() - 8.0 - 170.0, rect.top()),
            Vec2::new(170.0, rect.height()),
        );
        if sort_rect.left() > x + 80.0 {
            let r = hit(ui, sort_rect, Id::new("sort"));
            hover_fill(&p, &r, sort_rect);
            p.rect_stroke(sort_rect, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
            p.text(
                sort_rect.left_center() + Vec2::new(12.0, 0.0),
                Align2::LEFT_CENTER,
                self.sort.label(),
                FontId::proportional(14.0),
                TEXT,
            );
            let t = sort_rect.right_center() - Vec2::new(16.0, 0.0);
            p.add(egui::Shape::convex_polygon(
                vec![t + Vec2::new(-5.0, -2.5), t + Vec2::new(5.0, -2.5), t + Vec2::new(0.0, 3.5)],
                TEXT,
                Stroke::NONE,
            ));
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(sort_rect.width() - 12.0);
                for sort in Sort::ALL {
                    if ui.selectable_label(self.sort == sort, sort.label()).clicked() {
                        self.sort = sort;
                    }
                }
            });
            let g = galley(ui, "SORT", mono(10.0), Color32::PLACEHOLDER, 2.0);
            p.galley(
                Pos2::new(sort_rect.left() - 12.0 - g.size().x, rect.center().y - g.size().y / 2.0),
                g,
                FAINT,
            );
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.reap();
        self.drive_updates();
        // Keeps "checked … ago" fresh and wakes up for the hourly check.
        ctx.request_repaint_after(Duration::from_secs(30));
        if !self.running.is_empty() {
            // Keep "running" badges fresh without burning CPU.
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        let rows: Vec<Row> = self.manifest.apps.iter().enumerate().map(|(i, a)| self.row(i + 1, a)).collect();
        let mut cmds = Vec::new();

        self.title_bar(ui, &mut cmds);
        self.footer(ui, &mut cmds);

        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(20)))
            .show(ui, |ui| {
                self.toolbar(ui, &rows);
                ui.add_space(24.0);

                if let Some(err) = &self.load_error {
                    ui.colored_label(RED, format!("Could not load apps.json\n{err}"));
                    return;
                }
                let visible = self.visible(&rows);
                if visible.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new("NO APPS MATCH").font(mono(12.0)).color(MUTED).extra_letter_spacing(2.0))
                    });
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.view {
                    View::Grid => grid(ui, &visible, &mut cmds),
                    View::Table => table(ui, &visible, &mut cmds),
                });
            });

        window_frame(ui);

        for cmd in cmds {
            self.run(cmd);
        }
    }
}

fn grid(ui: &mut egui::Ui, rows: &[&Row], cmds: &mut Vec<Cmd>) {
    let width = ui.available_width().floor() - 1.0;
    let cols = ((width / CARD_MIN_WIDTH).floor() as usize).max(1);
    let card_w = (width / cols as f32).floor();
    let lines = rows.len().div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(Vec2::new(width, lines as f32 * CARD_HEIGHT + 1.0), Sense::hover());
    for (i, row) in rows.iter().enumerate() {
        let min = area.min + Vec2::new((i % cols) as f32 * card_w, (i / cols) as f32 * CARD_HEIGHT) + Vec2::splat(0.5);
        card(ui, Rect::from_min_size(min, Vec2::new(card_w, CARD_HEIGHT)), row, cmds);
    }
}

fn card(ui: &mut egui::Ui, rect: Rect, row: &Row, cmds: &mut Vec<Cmd>) {
    let id = Id::new(("card", &row.app.id));
    let p = ui.painter().clone();
    if ui.rect_contains_pointer(rect) {
        p.rect_filled(rect, 0, HOVER);
    }
    p.rect_stroke(rect, 0, Stroke::new(1.0, LINE), StrokeKind::Middle);
    let accent = row.accent();
    let pad = 20.0;

    // Header: number and links.
    let head = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 40.0));
    p.hline(rect.x_range(), head.bottom(), Stroke::new(1.0, LINE));
    number_badge(ui, &p, Pos2::new(rect.left() + pad, head.center().y), row.number, accent);
    let mut x = rect.right() - pad;
    if let Some(url) = row.app.github_url() {
        x = link(ui, Pos2::new(x, head.center().y), "GITHUB", id.with("gh"), &url, cmds) - 16.0;
    }
    if let Some(url) = row.app.website_url() {
        link(ui, Pos2::new(x, head.center().y), "WEBSITE", id.with("web"), url, cmds);
    }

    // Icon, name and description.
    let icon = Rect::from_min_size(Pos2::new(rect.left() + pad, head.bottom() + 18.0), Vec2::splat(56.0));
    paint_icon(ui, &p, icon, &row.app, accent, 12);
    let tx = icon.right() + 14.0;
    let text_w = rect.right() - pad - tx;
    let name = name_galley(ui, &row.app.name, 23.0, accent, text_w);
    p.galley(Pos2::new(tx, icon.top() + 1.0), name, TEXT);
    let desc = text_job(ui, row.app.description.clone(), FontId::proportional(14.5), Color32::PLACEHOLDER, 0.0, text_w);
    p.galley(Pos2::new(tx, icon.top() + 33.0), desc, MUTED);

    // Status chip and version.
    let (text, color) = row.chip();
    let chip_rect = chip(ui, &p, Pos2::new(rect.left() + pad, icon.bottom() + 24.0), &text, color);
    if let Some(err) = row.error() {
        ui.interact(chip_rect, id.with("chip"), Sense::hover()).on_hover_text(err);
    }
    let version = text_job(ui, row.version_line(), FontId::proportional(13.5), Color32::PLACEHOLDER, 0.0, rect.width() - 2.0 * pad);
    p.galley(Pos2::new(rect.left() + pad, chip_rect.bottom() + 16.0), version, SOFT);

    // Actions.
    let y = rect.bottom() - pad - 34.0;
    let menu = Rect::from_min_size(Pos2::new(rect.right() - pad - 38.0, y), Vec2::new(38.0, 34.0));
    let main = Rect::from_min_max(Pos2::new(rect.left() + pad, y), Pos2::new(menu.left() - 8.0, menu.bottom()));
    action_buttons(ui, main, menu, row, id, cmds);
}

fn table(ui: &mut egui::Ui, rows: &[&Row], cmds: &mut Vec<Cmd>) {
    let width = ui.available_width().floor() - 1.0;
    let (head, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
    let p = ui.painter().clone();
    let left = head.left();
    let status_x = left + (width * 0.42).max(340.0);
    let version_x = left + width * 0.64;
    let show_version = width >= 1000.0;
    let mut headers = vec![(left + 16.0, "#"), (left + 120.0, "APP"), (status_x, "STATUS")];
    if show_version {
        headers.push((version_x, "VERSION"));
    }
    for (x, label) in headers {
        let g = galley(ui, label, mono(10.0), Color32::PLACEHOLDER, 2.0);
        p.galley(Pos2::new(x, head.center().y - g.size().y / 2.0), g, FAINT);
    }

    let top = head.bottom();
    for row in rows {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::hover());
        let id = Id::new(("row", &row.app.id));
        if ui.rect_contains_pointer(rect) {
            p.rect_filled(rect, 0, HOVER);
        }
        p.hline(rect.x_range(), rect.top(), Stroke::new(1.0, LINE));
        let cy = rect.center().y;
        let accent = row.accent();

        number_badge(ui, &p, Pos2::new(rect.left() + 16.0, cy), row.number, accent);
        let icon = Rect::from_center_size(Pos2::new(rect.left() + 84.0, cy), Vec2::splat(40.0));
        paint_icon(ui, &p, icon, &row.app, accent, 9);
        let text_w = status_x - (left + 120.0) - 20.0;
        let name = name_galley(ui, &row.app.name, 17.0, accent, text_w);
        p.galley(Pos2::new(left + 120.0, cy - 19.0), name, TEXT);
        let desc = text_job(ui, row.app.description.clone(), FontId::proportional(13.0), Color32::PLACEHOLDER, 0.0, text_w);
        p.galley(Pos2::new(left + 120.0, cy + 3.0), desc, MUTED);

        let (text, color) = row.chip();
        let g = galley(ui, text.as_str(), mono(10.0), Color32::PLACEHOLDER, 1.5);
        let chip_rect = chip(ui, &p, Pos2::new(status_x, cy - (g.size().y + 8.0) / 2.0), &text, color);
        if let Some(err) = row.error() {
            ui.interact(chip_rect, id.with("chip"), Sense::hover()).on_hover_text(err);
        }
        if show_version {
            let v = text_job(ui, row.version_line(), FontId::proportional(13.0), Color32::PLACEHOLDER, 0.0, width * 0.2);
            p.galley(Pos2::new(version_x, cy - v.size().y / 2.0), v, SOFT);
        }

        let menu = Rect::from_center_size(Pos2::new(rect.right() - 16.0 - 17.0, cy), Vec2::new(34.0, 32.0));
        let main = Rect::from_min_max(Pos2::new(menu.left() - 8.0 - 140.0, menu.top()), Pos2::new(menu.left() - 8.0, menu.bottom()));
        action_buttons(ui, main, menu, row, id, cmds);
    }
    let bottom = ui.min_rect().bottom();
    p.rect_stroke(
        Rect::from_x_y_ranges(left + 0.5..=left + width, top..=bottom),
        0,
        Stroke::new(1.0, LINE),
        StrokeKind::Inside,
    );
}

fn action_buttons(ui: &mut egui::Ui, main: Rect, menu: Rect, row: &Row, id: Id, cmds: &mut Vec<Cmd>) {
    let (label, filled, enabled, progress) = row.primary();
    if button(ui, main, id.with("main"), &label, filled, enabled, progress).clicked()
        && let Some(cmd) = row.primary_cmd()
    {
        cmds.push(cmd);
    }
    let more = button(ui, menu, id.with("more"), "...", false, true, None);
    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(190.0);
        let app = &row.app;
        if matches!(row.phase, Phase::Ready) && ui.button("Open").clicked() {
            cmds.push(Cmd::Open(app.clone()));
        }
        if app.github.is_some() {
            if ui.button("Check for updates").clicked() {
                cmds.push(Cmd::Check(app.clone()));
            }
            if row.installed {
                if ui.button("Show install folder").clicked() {
                    cmds.push(Cmd::Reveal(updater::install_folder(&app.id)));
                }
                let uninstall = ui
                    .add_enabled(row.running == 0, egui::Button::new("Uninstall"))
                    .on_disabled_hover_text("Close the app first");
                if uninstall.clicked() {
                    cmds.push(Cmd::Uninstall(app.clone()));
                }
            }
        }
        ui.separator();
        if let Some(url) = app.website_url()
            && ui.button("Website").clicked()
        {
            cmds.push(Cmd::Url(url.to_string()));
        }
        if let Some(url) = app.github_url()
            && ui.button("GitHub page").clicked()
        {
            cmds.push(Cmd::Url(url));
        }
    });
}

fn button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: Id,
    label: &str,
    filled: bool,
    enabled: bool,
    progress: Option<f32>,
) -> Response {
    let mut r = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    if enabled {
        r = r.on_hover_cursor(CursorIcon::PointingHand);
    }
    let hovered = enabled && r.hovered();
    let (fill, stroke, fg) = match (filled, enabled) {
        (true, _) => (if hovered { Color32::from_rgb(212, 212, 215) } else { TEXT }, TEXT, BG),
        (false, true) => (if hovered { HOVER } else { Color32::TRANSPARENT }, if hovered { SOFT } else { LINE }, TEXT),
        (false, false) => (Color32::TRANSPARENT, LINE, MUTED),
    };
    let p = ui.painter();
    p.rect(rect, 0, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    if let Some(done) = progress {
        let mut bar = rect.shrink(1.0);
        bar.set_width(bar.width() * done.clamp(0.0, 1.0));
        p.rect_filled(bar, 0, BLUE.gamma_multiply(0.22));
    }
    let g = galley(ui, label, mono(11.0), Color32::PLACEHOLDER, 2.0);
    p.galley(rect.center() - g.size() / 2.0, g, fg);
    r
}

/// `WEBSITE ↗`-style link whose right edge sits at `right_center`. Returns its left edge.
fn link(ui: &mut egui::Ui, right_center: Pos2, text: &str, id: Id, url: &str, cmds: &mut Vec<Cmd>) -> f32 {
    let g = galley(ui, text, mono(10.5), Color32::PLACEHOLDER, 1.8);
    let arrow = 6.0;
    let size = Vec2::new(g.size().x + 7.0 + arrow, g.size().y);
    let rect = Rect::from_min_size(right_center - Vec2::new(size.x, size.y / 2.0), size);
    let r = ui
        .interact(rect.expand(3.0), id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(url);
    let color = if r.hovered() { TEXT } else { SOFT };
    let p = ui.painter();
    p.galley(rect.min, g, color);
    let a = Rect::from_min_size(Pos2::new(rect.right() - arrow, rect.center().y - arrow / 2.0 - 1.0), Vec2::splat(arrow));
    let s = Stroke::new(1.2, color);
    p.line_segment([a.left_bottom(), a.right_top()], s);
    p.line_segment([a.right_top(), a.right_top() - Vec2::new(arrow * 0.7, 0.0)], s);
    p.line_segment([a.right_top(), a.right_top() + Vec2::new(0.0, arrow * 0.7)], s);
    if r.clicked() {
        cmds.push(Cmd::Url(url.to_string()));
    }
    rect.left()
}

fn number_badge(ui: &egui::Ui, p: &Painter, left_center: Pos2, number: usize, accent: Color32) {
    let g = galley(ui, format!("{number:02}"), mono(11.0), Color32::PLACEHOLDER, 1.0);
    let rect = Rect::from_min_size(left_center - Vec2::new(0.0, 10.0), Vec2::new(g.size().x + 14.0, 20.0));
    p.rect_filled(rect, 0, accent);
    p.galley(rect.center() - g.size() / 2.0, g, BAR);
}

fn chip(ui: &egui::Ui, p: &Painter, left_top: Pos2, text: &str, color: Color32) -> Rect {
    let g = galley(ui, text, mono(10.0), Color32::PLACEHOLDER, 1.5);
    let rect = Rect::from_min_size(left_top, g.size() + Vec2::new(12.0, 8.0));
    p.rect_stroke(rect, 0, Stroke::new(1.0, color.gamma_multiply(0.45)), StrokeKind::Inside);
    p.galley(rect.min + Vec2::new(6.0, 4.0), g, color);
    rect
}

fn paint_icon(ui: &egui::Ui, p: &Painter, rect: Rect, app: &AppEntry, accent: Color32, radius: u8) {
    if let Some(icon_file) = &app.icon {
        egui::Image::new(file_uri(&manifest::icon_path(icon_file)))
            .corner_radius(CornerRadius::same(radius))
            .paint_at(ui, rect);
    } else {
        p.rect_filled(rect, CornerRadius::same(radius), accent);
        let letter = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
        p.text(rect.center(), Align2::CENTER_CENTER, letter, bold(rect.height() * 0.45), Color32::WHITE);
    }
}

/// App name in bold; for multi-word names the last word takes the accent
/// color, like "TechSouls *Website*".
fn name_galley(ui: &egui::Ui, name: &str, size: f32, accent: Color32, max_width: f32) -> Arc<Galley> {
    let (head, tail) = match name.rsplit_once(' ') {
        Some((head, tail)) => (format!("{head} "), tail),
        None => (name.to_string(), ""),
    };
    let mut job = LayoutJob::default();
    for (text, color) in [(head.as_str(), TEXT), (tail, accent)] {
        if !text.is_empty() {
            job.append(text, 0.0, TextFormat { font_id: bold(size), color, ..Default::default() });
        }
    }
    job.wrap = TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

fn galley(ui: &egui::Ui, text: impl Into<String>, font: FontId, color: Color32, spacing: f32) -> Arc<Galley> {
    text_job(ui, text, font, color, spacing, f32::INFINITY)
}

/// One line of text, cut with "…" when wider than `max_width`. Use
/// `Color32::PLACEHOLDER` to pick the color when painting.
fn text_job(ui: &egui::Ui, text: impl Into<String>, font: FontId, color: Color32, spacing: f32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.into(),
        TextFormat { font_id: font, color, extra_letter_spacing: spacing, ..Default::default() },
    );
    job.wrap = TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn hit(ui: &egui::Ui, rect: Rect, id: Id) -> Response {
    ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand)
}

fn hover_fill(p: &Painter, r: &Response, rect: Rect) {
    if r.hovered() {
        p.rect_filled(rect, 0, HOVER);
    }
}

/// Window border plus edge handles to resize the undecorated window.
fn window_frame(ui: &egui::Ui) {
    let ctx = ui.ctx();
    let screen = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("window-border")))
        .rect_stroke(screen, 0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let (t, c) = (5.0, 12.0);
    let (l, r, top, b) = (screen.left(), screen.right(), screen.top(), screen.bottom());
    let zones = [
        (ResizeDirection::NorthWest, Rect::from_min_max(Pos2::new(l, top), Pos2::new(l + c, top + c)), CursorIcon::ResizeNorthWest),
        (ResizeDirection::NorthEast, Rect::from_min_max(Pos2::new(r - c, top), Pos2::new(r, top + c)), CursorIcon::ResizeNorthEast),
        (ResizeDirection::SouthWest, Rect::from_min_max(Pos2::new(l, b - c), Pos2::new(l + c, b)), CursorIcon::ResizeSouthWest),
        (ResizeDirection::SouthEast, Rect::from_min_max(Pos2::new(r - c, b - c), Pos2::new(r, b)), CursorIcon::ResizeSouthEast),
        (ResizeDirection::North, Rect::from_min_max(Pos2::new(l + c, top), Pos2::new(r - c, top + t)), CursorIcon::ResizeNorth),
        (ResizeDirection::South, Rect::from_min_max(Pos2::new(l + c, b - t), Pos2::new(r - c, b)), CursorIcon::ResizeSouth),
        (ResizeDirection::West, Rect::from_min_max(Pos2::new(l, top + c), Pos2::new(l + t, b - c)), CursorIcon::ResizeWest),
        (ResizeDirection::East, Rect::from_min_max(Pos2::new(r - t, top + c), Pos2::new(r, b - c)), CursorIcon::ResizeEast),
    ];
    for (i, (dir, rect, cursor)) in zones.into_iter().enumerate() {
        let resp = ui.interact(rect, Id::new(("resize", i)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(cursor);
        }
        if resp.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

fn exe_command(app: &AppEntry) -> Command {
    if app.github.is_none() {
        return Command::new(&app.target);
    }
    match updater::installation(&app.id) {
        Some(Install::Deb { exe, .. }) => return Command::new(exe),
        Some(Install::App { path }) => {
            // `-W` keeps `open` alive while the app runs, so it shows as running.
            let mut cmd = Command::new("open");
            cmd.args(["-W", "-n"]).arg(path);
            if !app.args.is_empty() {
                cmd.arg("--args");
            }
            return cmd;
        }
        _ => {}
    }
    let dir = updater::current_dir(&app.id);
    let exe = dir.join(&app.target);
    // Zips built off Linux may drop the executable bit.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&exe) {
            let mut perms = meta.permissions();
            if perms.mode() & 0o111 == 0 {
                perms.set_mode(perms.mode() | 0o755);
                let _ = std::fs::set_permissions(&exe, perms);
            }
        }
    }
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir);
    cmd
}

/// `file://` URI in the form egui's file loader expects: on Windows it must be
/// `file:///C:/...`, otherwise the path is treated as a UNC network share.
fn file_uri(path: &std::path::Path) -> String {
    let path = path.display().to_string();
    if cfg!(windows) {
        format!("file:///{}", path.replace('\\', "/"))
    } else {
        format!("file://{path}")
    }
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// `2026-10-12T08:00:00Z` → `Oct 12, 2026`.
fn format_date(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = iso.get(..10)?.split('-');
    let year = parts.next()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    Some(format!("{} {day}, {year}", MONTHS.get(month.checked_sub(1)?)?))
}

fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => "JUST NOW".into(),
        s @ 60..3600 => format!("{} MIN AGO", s / 60),
        s => format!("{} H AGO", s / 3600),
    }
}

fn platform_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "MACOS",
        "windows" => "WINDOWS",
        "linux" => "LINUX",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "X64",
        "aarch64" => "ARM64",
        "x86" => "X86",
        other => other,
    };
    format!("{os} {arch}").to_uppercase()
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BAR;
    v.extreme_bg_color = BAR;
    v.faint_bg_color = BG;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.selection.bg_fill = BRAND;
    v.selection.stroke = Stroke::new(1.0, SOFT);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::ZERO;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, SOFT);
    ctx.set_visuals(v);
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    });
}

/// Uses Segoe UI on Windows when available; egui's built-in fonts otherwise.
/// Always defines the `bold` family used for app names.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font_dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let load = |files: &[&str]| files.iter().find_map(|f| std::fs::read(font_dir.join(f)).ok());

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    let mut bold_family = proportional.clone();
    if let Some(bytes) = load(&["segoeui.ttf"]) {
        fonts.font_data.insert("ui".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    if let Some(bytes) = load(&["seguisb.ttf", "segoeuib.ttf"]) {
        fonts.font_data.insert("ui-bold".into(), Arc::new(egui::FontData::from_owned(bytes)));
        bold_family.insert(0, "ui-bold".into());
    }
    fonts.families.insert(FontFamily::Name("bold".into()), bold_family);
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_release_dates() {
        assert_eq!(format_date("2026-10-12T08:00:00Z").as_deref(), Some("Oct 12, 2026"));
        assert_eq!(format_date("2026-01-03").as_deref(), Some("Jan 3, 2026"));
        assert_eq!(format_date(""), None);
        assert_eq!(format_date("2026-13-01"), None);
    }
}
