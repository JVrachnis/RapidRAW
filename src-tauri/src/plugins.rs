//! Plugin system v1 (see docs/superpowers/specs/2026-07-09-plugin-system-design.md).
//!
//! Plugins are ES-module JS packages living under `<app-data>/plugins/<id>/`.
//! Rust's job is limited to two things: enumerate + validate manifests, and
//! serve an entry-point file's source text back to the frontend loader (which
//! does the actual `import(blobUrl)`). No sandboxing in v1 - this is trusted,
//! personal-fork execution - but path traversal is still guarded because a
//! malformed manifest could otherwise be used to read arbitrary files off disk.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Manifest as reported to the frontend. `error` is set (and other fields are
/// best-effort / possibly empty) when `plugin.json` is missing, malformed, or
/// missing required fields - the UI surfaces this rather than silently
/// dropping the plugin.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    #[serde(default)]
    pub surfaces: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Raw, fully-optional shape used to parse `plugin.json` so that a manifest
/// missing a required field is a validation error we can report, not a hard
/// parse failure that discards everything we do know.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawPluginManifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    surfaces: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

/// Build a validated manifest for a plugin directory. `id` is always the
/// directory name (not whatever `plugin.json` might claim) since that's what
/// actually addresses the plugin on disk - keeping the two in sync avoids a
/// whole class of "id doesn't match its folder" path-resolution bugs.
fn build_manifest(dir_name: &str, raw: RawPluginManifest) -> PluginManifest {
    let name = non_empty(raw.name);
    let version = non_empty(raw.version);
    let entry = non_empty(raw.entry);

    let mut missing = Vec::new();
    if name.is_none() {
        missing.push("name");
    }
    if version.is_none() {
        missing.push("version");
    }
    if entry.is_none() {
        missing.push("entry");
    }

    if !missing.is_empty() {
        return PluginManifest {
            id: dir_name.to_string(),
            name: name.unwrap_or_default(),
            version: version.unwrap_or_default(),
            entry: entry.unwrap_or_default(),
            surfaces: raw.surfaces,
            description: raw.description,
            error: Some(format!(
                "plugin.json missing required field(s): {}",
                missing.join(", ")
            )),
        };
    }

    PluginManifest {
        id: dir_name.to_string(),
        name: name.unwrap(),
        version: version.unwrap(),
        entry: entry.unwrap(),
        surfaces: raw.surfaces,
        description: raw.description,
        error: None,
    }
}

fn invalid_manifest(dir_name: &str, message: String) -> PluginManifest {
    PluginManifest {
        id: dir_name.to_string(),
        name: String::new(),
        version: String::new(),
        entry: String::new(),
        surfaces: Vec::new(),
        description: None,
        error: Some(message),
    }
}

/// Parse a `plugin.json` file's raw text into a validated manifest. Pulled
/// out as a pure function so it's unit-testable without touching the
/// filesystem.
fn parse_manifest(dir_name: &str, content: &str) -> PluginManifest {
    match serde_json::from_str::<RawPluginManifest>(content) {
        Ok(raw) => build_manifest(dir_name, raw),
        Err(e) => invalid_manifest(dir_name, format!("Invalid plugin.json: {}", e)),
    }
}

/// `<app-data>/plugins`, created on demand.
pub fn plugins_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("plugins");

    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }

    Ok(dir)
}

#[tauri::command]
pub fn list_plugins(app_handle: AppHandle) -> Vec<PluginManifest> {
    let dir = match plugins_dir(&app_handle) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut manifests = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };

        let manifest_path = path.join("plugin.json");
        let manifest = match fs::read_to_string(&manifest_path) {
            Ok(content) => parse_manifest(&dir_name, &content),
            Err(e) => invalid_manifest(&dir_name, format!("Missing or unreadable plugin.json: {}", e)),
        };

        manifests.push(manifest);
    }

    manifests
}

/// Pure traversal guard: resolve `<plugins_dir>/<plugin_id>/<entry>` and
/// require the canonicalized result to still live under the canonicalized
/// plugins dir. Factored out from the command so it's testable without an
/// `AppHandle`.
pub fn resolve_entry_path(plugins_dir: &Path, plugin_id: &str, entry: &str) -> Result<PathBuf, String> {
    if plugin_id.is_empty() || plugin_id.contains('/') || plugin_id.contains('\\') || plugin_id.contains("..") {
        return Err("Invalid plugin id".to_string());
    }

    let canonical_plugins_dir = plugins_dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve plugins directory: {}", e))?;

    let candidate = plugins_dir.join(plugin_id).join(entry);

    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|e| format!("Entry file not found: {}", e))?;

    if !canonical_candidate.starts_with(&canonical_plugins_dir) {
        return Err("Access denied: entry path escapes the plugins directory".to_string());
    }

    Ok(canonical_candidate)
}

#[tauri::command]
pub fn read_plugin_entry(app_handle: AppHandle, plugin_id: String) -> Result<String, String> {
    let dir = plugins_dir(&app_handle)?;

    // Validate the id BEFORE any filesystem access so a malicious id cannot
    // even probe paths outside the plugins dir (defense in depth; the entry
    // path itself is separately guarded by resolve_entry_path below).
    if plugin_id.is_empty()
        || plugin_id.contains("..")
        || plugin_id.contains('/')
        || plugin_id.contains('\\')
    {
        return Err("Invalid plugin id".to_string());
    }

    let manifest_path = dir.join(&plugin_id).join("plugin.json");
    let content =
        fs::read_to_string(&manifest_path).map_err(|e| format!("Cannot read plugin manifest: {}", e))?;

    let raw: RawPluginManifest =
        serde_json::from_str(&content).map_err(|e| format!("Invalid plugin.json: {}", e))?;

    let entry = non_empty(raw.entry).ok_or_else(|| "plugin.json missing 'entry' field".to_string())?;

    let resolved = resolve_entry_path(&dir, &plugin_id, &entry)?;

    fs::read_to_string(&resolved).map_err(|e| format!("Cannot read plugin entry file: {}", e))
}

/// Opens the plugins directory in the OS file manager (Settings' "Open
/// plugins folder" button). Mirrors `file_management::show_in_finder`'s
/// per-OS dispatch, but opens the directory itself rather than selecting a
/// file within it.
#[tauri::command]
pub fn open_plugins_dir(app_handle: AppHandle) -> Result<(), String> {
    let dir = plugins_dir(&app_handle)?;

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "android")]
    {
        return Err("Opening folders is not supported on Android.".to_string());
    }

    #[cfg(target_os = "ios")]
    {
        return Err("Opening folders is not supported on iOS.".to_string());
    }

    Ok(())
}

// Bundled in the binary via `include_str!` so the repo's `plugin-examples/`
// sources are the single source of truth - "Install example plugins" just
// copies these bytes onto disk, it never diverges from what ships in the repo.
const EXIF_AUTO_PRESET_MANIFEST: &str = include_str!("../../plugin-examples/exif-auto-preset/plugin.json");
const EXIF_AUTO_PRESET_ENTRY: &str = include_str!("../../plugin-examples/exif-auto-preset/index.js");
const GATEWAY_MASK_TOOLS_MANIFEST: &str = include_str!("../../plugin-examples/gateway-mask-tools/plugin.json");
const GATEWAY_MASK_TOOLS_ENTRY: &str = include_str!("../../plugin-examples/gateway-mask-tools/index.js");

/// (dir name, plugin.json contents, index.js contents) for every bundled
/// example plugin.
const EXAMPLE_PLUGINS: &[(&str, &str, &str)] = &[
    ("exif-auto-preset", EXIF_AUTO_PRESET_MANIFEST, EXIF_AUTO_PRESET_ENTRY),
    ("gateway-mask-tools", GATEWAY_MASK_TOOLS_MANIFEST, GATEWAY_MASK_TOOLS_ENTRY),
];

/// Writes every bundled example plugin into `<app-data>/plugins/<id>/`,
/// overwriting any existing copy. Returns the ids that were (re)installed.
#[tauri::command]
pub fn install_example_plugins(app_handle: AppHandle) -> Result<Vec<String>, String> {
    let dir = plugins_dir(&app_handle)?;
    let mut installed = Vec::new();

    for (id, manifest, entry) in EXAMPLE_PLUGINS {
        let plugin_dir = dir.join(id);
        fs::create_dir_all(&plugin_dir).map_err(|e| format!("Failed to create {} directory: {}", id, e))?;
        fs::write(plugin_dir.join("plugin.json"), manifest)
            .map_err(|e| format!("Failed to write {}/plugin.json: {}", id, e))?;
        fs::write(plugin_dir.join("index.js"), entry)
            .map_err(|e| format!("Failed to write {}/index.js: {}", id, e))?;
        installed.push(id.to_string());
    }

    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_manifest() {
        let json = r#"{
            "id": "gateway-mask-tools",
            "name": "Gateway Mask Tools",
            "version": "1.0.0",
            "entry": "index.js",
            "surfaces": ["panel"],
            "description": "Adds gateway mask controls"
        }"#;

        let manifest = parse_manifest("gateway-mask-tools", json);

        assert_eq!(manifest.id, "gateway-mask-tools");
        assert_eq!(manifest.name, "Gateway Mask Tools");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.entry, "index.js");
        assert_eq!(manifest.surfaces, vec!["panel".to_string()]);
        assert_eq!(manifest.description, Some("Adds gateway mask controls".to_string()));
        assert!(manifest.error.is_none());
    }

    #[test]
    fn id_is_always_the_directory_name() {
        // Even if plugin.json disagrees (or omits it entirely), the id must
        // track the folder it was loaded from - that's what read_plugin_entry
        // uses to resolve paths.
        let json = r#"{"id":"totally-different","name":"X","version":"1.0.0","entry":"index.js"}"#;
        let manifest = parse_manifest("actual-dir-name", json);
        assert_eq!(manifest.id, "actual-dir-name");
    }

    #[test]
    fn missing_required_field_produces_error_entry_not_a_panic() {
        let json = r#"{"name": "Incomplete Plugin"}"#;

        let manifest = parse_manifest("incomplete-plugin", json);

        assert_eq!(manifest.id, "incomplete-plugin");
        assert!(manifest.error.is_some());
        let err = manifest.error.unwrap();
        assert!(err.contains("version"));
        assert!(err.contains("entry"));
    }

    #[test]
    fn malformed_json_produces_error_entry_not_a_panic() {
        let manifest = parse_manifest("broken-plugin", "{ this is not json");

        assert_eq!(manifest.id, "broken-plugin");
        assert!(manifest.error.is_some());
        assert!(manifest.error.unwrap().contains("Invalid plugin.json"));
    }

    #[test]
    fn blank_required_fields_are_treated_as_missing() {
        let json = r#"{"name": "  ", "version": "1.0.0", "entry": "index.js"}"#;

        let manifest = parse_manifest("blank-name-plugin", json);

        assert!(manifest.error.is_some());
        assert!(manifest.error.unwrap().contains("name"));
    }

    fn setup_plugin_dir(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_valid_entry_within_plugin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = setup_plugin_dir(tmp.path(), "good-plugin");
        fs::write(plugin_dir.join("index.js"), "export default () => {};").unwrap();

        let resolved = resolve_entry_path(tmp.path(), "good-plugin", "index.js").unwrap();

        assert!(resolved.ends_with("good-plugin/index.js") || resolved.ends_with("good-plugin\\index.js"));
    }

    #[test]
    fn rejects_entry_traversal_via_dotdot_segments() {
        // secret.txt lives one level above the plugins dir itself, so
        // "../../secret.txt" from inside a plugin dir would genuinely resolve
        // to it on disk if we didn't guard against escaping the plugins root.
        let tmp = tempfile::tempdir().unwrap();
        let plugins_root = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_root).unwrap();
        fs::write(tmp.path().join("secret.txt"), "top secret").unwrap();
        let plugin_dir = setup_plugin_dir(&plugins_root, "evil-plugin");
        fs::write(plugin_dir.join("index.js"), "export default () => {};").unwrap();

        let result = resolve_entry_path(&plugins_root, "evil-plugin", "../../secret.txt");

        assert!(result.is_err());
    }

    #[test]
    fn rejects_plugin_id_containing_dotdot() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("secret.txt"), "top secret").unwrap();

        let result = resolve_entry_path(tmp.path(), "..", "secret.txt");

        assert!(result.is_err());
    }

    #[test]
    fn rejects_plugin_id_with_path_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_entry_path(tmp.path(), "some/where", "index.js");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_missing_entry_file_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        setup_plugin_dir(tmp.path(), "no-entry-plugin");

        let result = resolve_entry_path(tmp.path(), "no-entry-plugin", "index.js");

        assert!(result.is_err());
    }
}
