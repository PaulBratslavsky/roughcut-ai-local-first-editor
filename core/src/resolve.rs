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

fn resolve_app() -> Option<PathBuf> {
    [
        "/Applications/DaVinci Resolve.app",
        "/Applications/DaVinci Resolve/DaVinci Resolve.app",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_dir())
}

fn app_installed() -> bool {
    resolve_app().is_some()
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

const SCRIPTING_MODULES: &str =
    "/Library/Application Support/Blackmagic Design/DaVinci Resolve/Developer/Scripting/Modules";

/// Push an exported timeline XML into the RUNNING Resolve instance via the
/// documented scripting module (Resolve's bundled `fuscript` crashes
/// silently; plain python3 + DaVinciResolveScript is the reliable route).
/// Errors name the exact precondition that failed — Resolve closed, external
/// scripting off, no project open — so the UI can guide instead of shrug.
pub async fn send_timeline(xml_path: &str) -> Result<String> {
    let app = resolve_app().ok_or_else(|| {
        crate::error::CoreError::Unavailable("DaVinci Resolve is not installed".into())
    })?;
    let lib = app.join("Contents/Libraries/Fusion/fusionscript.so");
    let script = format!(
        r#"
import DaVinciResolveScript as dvr
xml = r"{xml}"
resolve = dvr.scriptapp("Resolve")
if not resolve:
    print("ERR:Resolve isn't reachable - is it running, and is Preferences > System > General > 'External scripting using' set to Local?")
else:
    pm = resolve.GetProjectManager()
    proj = pm.GetCurrentProject() if pm else None
    if not proj:
        print("ERR:open (or create) a project in Resolve first")
    else:
        tl = proj.GetMediaPool().ImportTimelineFromFile(xml)
        if tl:
            print("OK:" + tl.GetName())
        else:
            print("ERR:Resolve refused the timeline import")
"#,
        xml = xml_path.replace('"', "")
    );
    let tmp = std::env::temp_dir().join(format!("roughcut-resolve-{}.py", std::process::id()));
    std::fs::write(&tmp, script)?;
    // GUI apps don't inherit the shell PATH: prefer the system python3.
    let python = if PathBuf::from("/usr/bin/python3").is_file() {
        "/usr/bin/python3".to_string()
    } else {
        "python3".to_string()
    };
    let out = tokio::process::Command::new(python)
        .arg(&tmp)
        .env("RESOLVE_SCRIPT_LIB", &lib)
        .env("PYTHONPATH", SCRIPTING_MODULES)
        .output()
        .await
        .map_err(|e| crate::error::CoreError::Unavailable(format!("python3: {e}")))?;
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(name) = stdout.lines().find_map(|l| l.strip_prefix("OK:")) {
        return Ok(name.trim().to_string());
    }
    let err = stdout
        .lines()
        .find_map(|l| l.strip_prefix("ERR:"))
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "Resolve scripting failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()
            )
        });
    Err(crate::error::CoreError::Unavailable(err))
}
