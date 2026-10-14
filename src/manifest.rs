use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One launchable company app, as described in `apps.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    /// Executable path (or URL when `kind` is `url`). For `github` apps this
    /// is relative to the installed release folder.
    pub target: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub kind: LaunchKind,
    /// Accent color as `#RRGGBB`.
    #[serde(default)]
    pub color: Option<String>,
    /// Icon image (SVG/PNG), absolute or relative to the config folder.
    #[serde(default)]
    pub icon: Option<String>,
    /// `owner/repo` whose GitHub Releases provide this app; the launcher
    /// installs it and keeps it up to date.
    #[serde(default)]
    pub github: Option<String>,
    /// Substring that selects the release `.zip` (defaults to the OS name).
    #[serde(default)]
    pub asset: Option<String>,
    /// Product page, shown as the card's "Website" link.
    #[serde(default)]
    pub website: Option<String>,
}

impl AppEntry {
    pub fn website_url(&self) -> Option<&str> {
        self.website
            .as_deref()
            .or((self.kind == LaunchKind::Url).then_some(self.target.as_str()))
    }

    pub fn github_url(&self) -> Option<String> {
        self.github.as_ref().map(|repo| format!("https://github.com/{repo}"))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchKind {
    /// Spawn a native executable and track its process.
    #[default]
    Exe,
    /// Open with the OS default handler (URL, document, .app bundle, ...).
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub apps: Vec<AppEntry>,
}

fn default_category() -> String {
    "General".into()
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("techsouls-launcher")
}

pub fn manifest_path() -> PathBuf {
    config_dir().join("apps.json")
}

/// Icons shipped inside the launcher, written to `<config>/icons/`.
const BUNDLED_ICONS: &[(&str, &[u8])] = &[
    ("catplay.svg", include_bytes!("../assets/icons/catplay.svg")),
    ("yarita.svg", include_bytes!("../assets/icons/yarita.svg")),
];

/// Resolves an app's `icon` to a path on disk.
pub fn icon_path(icon: &str) -> PathBuf {
    config_dir().join(icon)
}

fn write_bundled_icons() {
    let dir = config_dir().join("icons");
    let _ = std::fs::create_dir_all(&dir);
    for (name, bytes) in BUNDLED_ICONS {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

/// Loads the built-in apps plus any extra apps from `apps.json` (created
/// empty on first run). Entries in `apps.json` override built-in apps with
/// the same `id`.
pub fn load() -> Result<Manifest, String> {
    let path = manifest_path();
    write_bundled_icons();
    if !path.exists() {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&Manifest { apps: vec![] }).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Windows editors often save UTF-8 with a BOM, which serde_json rejects.
    let user: Manifest =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(merge(builtin(), user))
}

fn merge(mut base: Manifest, user: Manifest) -> Manifest {
    for entry in user.apps {
        match base.apps.iter_mut().find(|a| a.id == entry.id) {
            Some(existing) => *existing = entry,
            None => base.apps.push(entry),
        }
    }
    base
}

/// Company apps that ship with the launcher.
fn builtin() -> Manifest {
    Manifest {
        apps: vec![
            AppEntry {
                id: "catplay".into(),
                name: "CAT".into(),
                description: "Fast desktop IPTV player".into(),
                category: "Media".into(),
                target: if cfg!(windows) { "CAT.exe" } else { "catplay" }.into(),
                args: vec![],
                kind: LaunchKind::Exe,
                color: Some("#1E7BF2".into()),
                icon: Some("icons/catplay.svg".into()),
                github: Some("technsoul/catplay".into()),
                asset: None,
                website: None,
            },
            AppEntry {
                id: "yarita".into(),
                name: "Yarita".into(),
                description: "Pomodoro timer that blocks distractions".into(),
                category: "Productivity".into(),
                target: if cfg!(windows) { "focus-ui.exe" } else { "focus-ui" }.into(),
                args: vec![],
                kind: LaunchKind::Exe,
                color: Some("#000000".into()),
                icon: Some("icons/yarita.svg".into()),
                github: Some("technsoul/yarita".into()),
                asset: None,
                website: None,
            },
            AppEntry {
                id: "website".into(),
                name: "TechSouls Website".into(),
                description: "www.techsouls.tech".into(),
                category: "Web".into(),
                target: "https://www.techsouls.tech".into(),
                args: vec![],
                kind: LaunchKind::Url,
                color: Some("#F59E0B".into()),
                icon: None,
                github: None,
                asset: None,
                website: None,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(m: &Manifest) -> Vec<&str> {
        m.apps.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn user_entries_override_and_extend_builtins() {
        let mut user = builtin();
        user.apps.truncate(1);
        user.apps[0].name = "CAT Beta".into();
        let mut extra = user.apps[0].clone();
        extra.id = "extra".into();
        extra.name = "Extra".into();
        user.apps.push(extra);

        let merged = merge(builtin(), user);
        assert_eq!(names(&merged), ["CAT Beta", "Yarita", "TechSouls Website", "Extra"]);
    }
}
