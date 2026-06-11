//! DaVinci Resolve integration status + plugin install. The script itself is
//! EMBEDDED in the binary (the packaged app has no repo checkout), so
//! installing is just writing it into Resolve's Scripts menu folder.

use crate::error::Result;
use serde::Serialize;
use std::path::PathBuf;

/// The script source, compiled in — updates ship with the app.
const PLUGIN: &str = include_str!("../../resolve-plugin/RoughCut AI Draft.py");
const PLUGIN_FILE: &str = "RoughCut AI Draft.py";

#[derive(Debug, Clone, Serialize)]
pub struct ResolveStatus {
    /// DaVinci Resolve.app found on this machine.
    pub app_installed: bool,
    /// Our script sits in Resolve's Scripts menu folder.
    pub plugin_installed: bool,
    /// The script is installed but older than the one shipped in this build.
    pub plugin_outdated: bool,
    pub scripts_dir: String,
}

fn scripts_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join("Library/Application Support/Blackmagic Design/DaVinci Resolve/Fusion/Scripts/Utility")
}

fn app_installed() -> bool {
    PathBuf::from("/Applications/DaVinci Resolve/DaVinci Resolve.app").is_dir()
        || PathBuf::from("/Applications/DaVinci Resolve.app").is_dir()
}

pub fn status() -> ResolveStatus {
    let dest = scripts_dir().join(PLUGIN_FILE);
    let installed_src = std::fs::read_to_string(&dest).ok();
    ResolveStatus {
        app_installed: app_installed(),
        plugin_installed: installed_src.is_some(),
        plugin_outdated: installed_src.map(|s| s != PLUGIN).unwrap_or(false),
        scripts_dir: scripts_dir().to_string_lossy().into_owned(),
    }
}

/// Write (or refresh) the script into Resolve's Scripts menu. Resolve picks
/// it up on next launch or via Workspace ▸ Scripts ▸ Refresh.
pub fn install_plugin() -> Result<String> {
    let dir = scripts_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(PLUGIN_FILE);
    std::fs::write(&dest, PLUGIN)?;
    Ok(dest.to_string_lossy().into_owned())
}
