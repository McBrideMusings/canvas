//! The Claude Code integration: installs or refreshes the plugin from a local
//! checkout, so the hooks and skill Claude Code runs always match the `canvas`
//! binary that was just deployed.
//!
//! The marketplace is registered as a `directory` source pointing at the
//! checkout, never a git remote, so the plugin always comes from the same
//! checkout the binary was built from.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::Status;

const MARKETPLACE: &str = "canvas";
const PLUGIN: &str = "canvas@canvas";

/// Compares the installed plugin version to the `plugin.json` in the checkout
/// the marketplace points at.
pub fn status() -> Result<Status, String> {
    let (Some(path), Some(installed)) = (marketplace_path()?, installed_version()?) else {
        return Ok(Status::NotInstalled);
    };
    let manifest = Path::new(&path).join("plugin/.claude-plugin/plugin.json");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("could not read {}: {e}", manifest.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("could not parse {}: {e}", manifest.display()))?;
    Ok(if json["version"].as_str() == Some(installed.as_str()) {
        Status::Current
    } else {
        Status::OutOfDate
    })
}

pub fn install(arg: Option<&str>) -> Result<(), String> {
    let repo = repo_dir(arg)?;
    let repo_str = repo.to_string_lossy().to_string();

    match marketplace_path()? {
        None => {
            claude(&["plugin", "marketplace", "add", &repo_str])?;
        }
        Some(path) if Path::new(&path) == repo => {
            claude(&["plugin", "marketplace", "update", MARKETPLACE])?;
        }
        Some(path) => {
            return Err(format!(
                "the {MARKETPLACE} marketplace points at {path}, not {repo_str}; \
                 run `claude plugin marketplace remove {MARKETPLACE}` first"
            ));
        }
    }

    if installed_version()?.is_some() {
        claude(&["plugin", "update", PLUGIN])?;
    } else {
        claude(&["plugin", "install", PLUGIN])?;
    }

    let version =
        installed_version()?.ok_or_else(|| format!("{PLUGIN} is not installed after install"))?;
    println!(
        "{PLUGIN} {version} installed from {repo_str}; restart Claude Code sessions to load it"
    );
    Ok(())
}

/// The checkout to install from: the argument, else the current directory.
/// It must carry the marketplace manifest.
fn repo_dir(arg: Option<&str>) -> Result<PathBuf, String> {
    let dir = match arg {
        Some(a) => PathBuf::from(a),
        None => std::env::current_dir()
            .map_err(|e| format!("could not resolve the current directory: {e}"))?,
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("could not resolve {}: {e}", dir.display()))?;
    if !dir.join(".claude-plugin/marketplace.json").is_file() {
        return Err(format!(
            "{} has no .claude-plugin/marketplace.json; pass the canvas checkout or run from it",
            dir.display()
        ));
    }
    Ok(dir)
}

/// The directory the `canvas` marketplace is registered at, if it is.
fn marketplace_path() -> Result<Option<String>, String> {
    let out = claude(&["plugin", "marketplace", "list", "--json"])?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&out)
        .map_err(|e| format!("could not parse `claude plugin marketplace list --json`: {e}"))?;
    Ok(list
        .iter()
        .find(|m| m["name"] == MARKETPLACE)
        .and_then(|m| m["path"].as_str())
        .map(str::to_string))
}

/// The installed version of the plugin, if it is installed.
fn installed_version() -> Result<Option<String>, String> {
    let out = claude(&["plugin", "list", "--json"])?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&out)
        .map_err(|e| format!("could not parse `claude plugin list --json`: {e}"))?;
    Ok(list
        .iter()
        .find(|p| p["id"] == PLUGIN)
        .and_then(|p| p["version"].as_str())
        .map(str::to_string))
}

/// Runs `claude <args>` and returns its stdout, or its stderr as the error.
fn claude(args: &[&str]) -> Result<String, String> {
    let out = Command::new("claude")
        .args(args)
        .output()
        .map_err(|e| format!("could not run claude: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "`claude {}` failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
