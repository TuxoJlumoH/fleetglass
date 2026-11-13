//! Plugin manifest validation and discovery engine (P3-04).

use std::path::{Path, PathBuf};

use crate::api::PluginManifest;

/// Allowed plugin permissions.
pub const ALLOWED_PERMISSIONS: &[&str] = &["read:resources", "read:logs", "read:contexts"];

/// Errors produced while validating or discovering plugins.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Plugin manifest or configuration is invalid.
    #[error("invalid plugin manifest: {0}")]
    InvalidInput(String),

    /// Directory or manifest file could not be read.
    #[error("failed to read plugin directory {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Result alias for plugin operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Checks if a plugin ID matches `^[a-z0-9]+(-[a-z0-9]+)*$` (non-empty, kebab-case).
fn is_valid_id(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    for segment in id.split('-') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return false;
        }
    }
    true
}

/// Maximum allowed plugin manifest size (1MB).
pub const MAX_PLUGIN_MANIFEST_BYTES: u64 = 1_048_576;

/// Validates a plugin manifest JSON string and parses it into a [`PluginManifest`].
pub fn validate_manifest(json: &str) -> Result<PluginManifest> {
    let manifest: PluginManifest = serde_json::from_str(json)
        .map_err(|err| Error::InvalidInput(format!("failed to parse plugin manifest: {err}")))?;

    // `localhost` is reserved: on Windows plugins are served as `fgplugin://localhost/<id>/`.
    if !is_valid_id(&manifest.id) || manifest.id == "localhost" {
        return Err(Error::InvalidInput(format!(
            "invalid plugin id '{}': must match ^[a-z0-9]+(-[a-z0-9]+)*$",
            manifest.id
        )));
    }

    if manifest.entrypoint.is_empty()
        || manifest.entrypoint.starts_with('/')
        || manifest.entrypoint.contains("..")
        || manifest.entrypoint.contains('\\')
        || manifest.entrypoint.contains(':')
    {
        return Err(Error::InvalidInput(format!(
            "invalid plugin entrypoint '{}': must not start with '/', contain '..', backslashes or ':'",
            manifest.entrypoint
        )));
    }

    for perm in &manifest.permissions {
        if !ALLOWED_PERMISSIONS.contains(&perm.as_str()) {
            return Err(Error::InvalidInput(format!(
                "unknown plugin permission '{perm}'"
            )));
        }
    }

    Ok(manifest)
}

/// A validated plugin together with the directory it was discovered in.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// The validated manifest.
    pub manifest: PluginManifest,
    /// The plugin's own (non-symlink) directory containing `manifest.json`.
    pub dir: PathBuf,
}

/// Loads and validates the single plugin directory `dir` (not a symlink, `manifest.json`
/// a regular file of at most [`MAX_PLUGIN_MANIFEST_BYTES`]). Applies the same rules as
/// [`discover_plugin_dirs`], so a cached plugin can be re-checked cheaply before use.
pub fn load_plugin_dir(dir: &Path) -> Result<DiscoveredPlugin> {
    let io = |path: &Path, source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let meta = std::fs::symlink_metadata(dir).map_err(|e| io(dir, e))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(Error::InvalidInput(format!(
            "{} is not a plugin directory (symlinks are not followed)",
            dir.display()
        )));
    }
    let manifest_path = dir.join("manifest.json");
    let meta = std::fs::metadata(&manifest_path).map_err(|e| io(&manifest_path, e))?;
    if !meta.is_file() || meta.len() > MAX_PLUGIN_MANIFEST_BYTES {
        return Err(Error::InvalidInput(format!(
            "{} is not a regular file of at most {MAX_PLUGIN_MANIFEST_BYTES} bytes",
            manifest_path.display()
        )));
    }
    let content = std::fs::read_to_string(&manifest_path).map_err(|e| io(&manifest_path, e))?;
    Ok(DiscoveredPlugin {
        manifest: validate_manifest(&content)?,
        dir: dir.to_path_buf(),
    })
}

/// Scans subdirectories of `dir` for plugin manifests (`manifest.json`), validates
/// and returns them sorted by plugin `id`. Unreadable directories or invalid manifests
/// are skipped gracefully with a warning.
pub fn discover_plugins(dir: &Path) -> Result<Vec<PluginManifest>> {
    Ok(discover_plugin_dirs(dir)?
        .into_iter()
        .map(|p| p.manifest)
        .collect())
}

/// Like [`discover_plugins`], but also returns each plugin's directory. Symlinked
/// entries, oversized manifests and duplicate ids are skipped.
pub fn discover_plugin_dirs(dir: &Path) -> Result<Vec<DiscoveredPlugin>> {
    let entries = std::fs::read_dir(dir).map_err(|source| Error::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut plugins = Vec::new();

    // Sorted so that duplicate ids resolve deterministically (first directory name wins).
    let mut entries: Vec<_> = entries.collect();
    entries.sort_by_key(|e| e.as_ref().map(|e| e.file_name()).ok());

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!("failed to read directory entry in {}: {err}", dir.display());
                continue;
            }
        };

        let sub_path = entry.path();

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                tracing::warn!(
                    "failed to inspect file type for {}: {err}",
                    sub_path.display()
                );
                continue;
            }
        };

        if file_type.is_symlink() {
            tracing::warn!(
                "skipping symlink entry in plugins directory: {}",
                sub_path.display()
            );
            continue;
        }

        if !file_type.is_dir() {
            continue;
        }

        let manifest_path = sub_path.join("manifest.json");
        match std::fs::metadata(&manifest_path) {
            Ok(meta) => {
                if !meta.is_file() || meta.len() > MAX_PLUGIN_MANIFEST_BYTES {
                    continue;
                }
            }
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        "failed to access plugin manifest at {}: {err}",
                        manifest_path.display()
                    );
                }
                continue;
            }
        }

        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(content) => content,
            Err(err) => {
                tracing::warn!(
                    "failed to read plugin manifest at {}: {err}",
                    manifest_path.display()
                );
                continue;
            }
        };

        match validate_manifest(&content) {
            Ok(manifest) => {
                if plugins
                    .iter()
                    .any(|p: &DiscoveredPlugin| p.manifest.id == manifest.id)
                {
                    tracing::warn!(
                        "skipping duplicate plugin id '{}' in {}",
                        manifest.id,
                        sub_path.display()
                    );
                } else {
                    plugins.push(DiscoveredPlugin {
                        manifest,
                        dir: sub_path,
                    });
                }
            }
            Err(err) => {
                tracing::warn!("skipping invalid plugin in {}: {err}", sub_path.display());
            }
        }
    }

    plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "fleetglass-plugins-test-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                // Ensure permissions allow directory removal
                use std::os::unix::fs::PermissionsExt;
                if let Ok(entries) = std::fs::read_dir(&self.0) {
                    for entry in entries.flatten() {
                        let _ = std::fs::set_permissions(
                            entry.path(),
                            std::fs::Permissions::from_mode(0o755),
                        );
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn valid_manifest_parses_successfully() {
        let json = r#"{
            "id": "resource-graph",
            "name": "Resource Graph",
            "version": "1.0.0",
            "description": "Visualizes Kubernetes resource relationships",
            "permissions": ["read:resources", "read:logs"],
            "entrypoint": "index.html"
        }"#;

        let manifest = validate_manifest(json).unwrap();
        assert_eq!(manifest.id, "resource-graph");
        assert_eq!(manifest.name, "Resource Graph");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(
            manifest.description,
            "Visualizes Kubernetes resource relationships"
        );
        assert_eq!(manifest.permissions, vec!["read:resources", "read:logs"]);
        assert_eq!(manifest.entrypoint, "index.html");
    }

    #[test]
    fn invalid_id_rejected_with_typed_error() {
        let invalid_ids = [
            "Resource-Graph",
            "resource graph",
            "resource_graph",
            "resource.graph",
            "resource@graph",
            "resource/graph",
            "",
            "-resource",
            "resource-",
            "resource--graph",
            "RESOURCE",
            "plug!n",
            "localhost",
        ];

        for invalid_id in invalid_ids {
            let json = format!(
                r#"{{
                    "id": "{invalid_id}",
                    "name": "Test Plugin",
                    "version": "0.1.0",
                    "description": "desc",
                    "permissions": ["read:resources"],
                    "entrypoint": "dist/index.html"
                }}"#
            );

            let err = validate_manifest(&json).unwrap_err();
            assert!(
                matches!(err, Error::InvalidInput(_)),
                "expected InvalidInput for id '{invalid_id}', got: {err:?}"
            );
        }
    }

    #[test]
    fn id_regex_matches_specification() {
        let re = regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").unwrap();

        let cases = [
            ("plugin", true),
            ("plugin-1", true),
            ("123", true),
            ("my-awesome-plugin-v2", true),
            ("a-b-c-d-e", true),
            ("", false),
            ("-plugin", false),
            ("plugin-", false),
            ("plug--in", false),
            ("Plugin", false),
            ("plug in", false),
            ("plug_in", false),
            ("plug.in", false),
            ("plug/in", false),
            ("plug@in", false),
        ];

        for (id, expected) in cases {
            assert_eq!(is_valid_id(id), expected, "id validation failed for '{id}'");
            assert_eq!(re.is_match(id), expected, "regex match mismatch for '{id}'");
        }
    }

    #[test]
    fn entrypoint_path_traversal_rejected() {
        let invalid_entrypoints = [
            "../escaped.html",
            "/abs.html",
            "/root/index.html",
            "sub/../../escaped.html",
            r"sub\escaped.html",
            r"\abs.html",
            "",
        ];

        for entrypoint in invalid_entrypoints {
            let json = format!(
                r#"{{
                    "id": "my-plugin",
                    "name": "My Plugin",
                    "version": "1.0.0",
                    "description": "desc",
                    "permissions": ["read:resources"],
                    "entrypoint": "{entrypoint}"
                }}"#
            );

            let err = validate_manifest(&json).unwrap_err();
            assert!(
                matches!(err, Error::InvalidInput(_)),
                "expected InvalidInput for entrypoint '{entrypoint}', got: {err:?}"
            );
        }
    }

    #[test]
    fn unknown_permission_rejected() {
        let invalid_perms_cases = [
            vec!["write:all"],
            vec!["exec:raw"],
            vec!["read:resources", "admin"],
            vec!["read:unknown"],
            vec!["read:resources", "write:pods"],
        ];

        for perms in invalid_perms_cases {
            let perms_json = serde_json::to_string(&perms).unwrap();
            let json = format!(
                r#"{{
                    "id": "my-plugin",
                    "name": "My Plugin",
                    "version": "1.0.0",
                    "description": "desc",
                    "permissions": {perms_json},
                    "entrypoint": "index.html"
                }}"#
            );

            let err = validate_manifest(&json).unwrap_err();
            assert!(
                matches!(err, Error::InvalidInput(_)),
                "expected InvalidInput for permissions {perms:?}, got: {err:?}"
            );
        }
    }

    #[test]
    fn directory_scanner_discovers_plugins_and_skips_invalid_ones() {
        let temp = TempDir::new("discovery");
        let dir = temp.path();

        // Valid plugin 1: "beta-plugin"
        let plugin_b = dir.join("dir-beta");
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(
            plugin_b.join("manifest.json"),
            r#"{
                "id": "beta-plugin",
                "name": "Beta Plugin",
                "version": "2.0.0",
                "description": "Beta",
                "permissions": ["read:logs"],
                "entrypoint": "main.html"
            }"#,
        )
        .unwrap();

        // Valid plugin 2: "alpha-plugin"
        let plugin_a = dir.join("dir-alpha");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::write(
            plugin_a.join("manifest.json"),
            r#"{
                "id": "alpha-plugin",
                "name": "Alpha Plugin",
                "version": "1.0.0",
                "description": "Alpha",
                "permissions": ["read:resources", "read:contexts"],
                "entrypoint": "dist/index.html"
            }"#,
        )
        .unwrap();

        // Invalid plugin 1: invalid json
        let invalid_json = dir.join("invalid-json");
        std::fs::create_dir_all(&invalid_json).unwrap();
        std::fs::write(invalid_json.join("manifest.json"), "{ invalid json }").unwrap();

        // Invalid plugin 2: invalid id
        let invalid_id = dir.join("invalid-id");
        std::fs::create_dir_all(&invalid_id).unwrap();
        std::fs::write(
            invalid_id.join("manifest.json"),
            r#"{
                "id": "INVALID_ID",
                "name": "Bad ID",
                "version": "1.0.0",
                "description": "Bad ID",
                "permissions": ["read:resources"],
                "entrypoint": "index.html"
            }"#,
        )
        .unwrap();

        // Invalid plugin 3: traversal entrypoint
        let traversal = dir.join("traversal");
        std::fs::create_dir_all(&traversal).unwrap();
        std::fs::write(
            traversal.join("manifest.json"),
            r#"{
                "id": "traversal-plugin",
                "name": "Traversal",
                "version": "1.0.0",
                "description": "desc",
                "permissions": ["read:resources"],
                "entrypoint": "../escape.html"
            }"#,
        )
        .unwrap();

        // Invalid plugin 4: unknown permission
        let unknown_perm = dir.join("unknown-perm");
        std::fs::create_dir_all(&unknown_perm).unwrap();
        std::fs::write(
            unknown_perm.join("manifest.json"),
            r#"{
                "id": "unknown-perm-plugin",
                "name": "Unknown Perm",
                "version": "1.0.0",
                "description": "desc",
                "permissions": ["write:all"],
                "entrypoint": "index.html"
            }"#,
        )
        .unwrap();

        // Non-plugin directory (no manifest.json)
        let empty_dir = dir.join("empty-dir");
        std::fs::create_dir_all(&empty_dir).unwrap();

        // Regular file in root
        std::fs::write(dir.join("readme.txt"), "hello").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let unreadable = dir.join("unreadable-dir");
            std::fs::create_dir_all(&unreadable).unwrap();
            let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000));
        }

        let discovered = discover_plugins(dir).unwrap();

        // Should contain alpha-plugin and beta-plugin, sorted by id
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].id, "alpha-plugin");
        assert_eq!(discovered[1].id, "beta-plugin");
    }

    #[test]
    fn entrypoint_colon_scheme_rejected() {
        let manifest = r#"{
            "id": "evil-plugin",
            "name": "Evil",
            "version": "1.0.0",
            "description": "desc",
            "permissions": ["read:resources"],
            "entrypoint": "https://evil.com/index.html"
        }"#;
        let err = validate_manifest(manifest).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let js_manifest = r#"{
            "id": "js-plugin",
            "name": "JS",
            "version": "1.0.0",
            "description": "desc",
            "permissions": ["read:resources"],
            "entrypoint": "javascript:alert(1)"
        }"#;
        let js_err = validate_manifest(js_manifest).unwrap_err();
        assert!(matches!(js_err, Error::InvalidInput(_)));
    }

    fn write_manifest(dir: &Path, id: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","name":"N","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn discovery_skips_symlinks_duplicates_and_oversized_manifests() {
        let tmp = TempDir::new("hardening");
        let dir = tmp.path();
        write_manifest(&dir.join("a"), "dup-plugin");
        write_manifest(&dir.join("b"), "dup-plugin");

        let big = dir.join("big");
        std::fs::create_dir_all(&big).unwrap();
        let padding = " ".repeat(MAX_PLUGIN_MANIFEST_BYTES as usize + 1);
        std::fs::write(
            big.join("manifest.json"),
            format!(
                r#"{{"id":"big-plugin","name":"N","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}}{padding}"#
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            let outside = TempDir::new("hardening-outside");
            write_manifest(&outside.path().join("p"), "linked-plugin");
            std::os::unix::fs::symlink(outside.path().join("p"), dir.join("linked")).unwrap();
            let found = discover_plugin_dirs(dir).unwrap();
            assert!(found.iter().all(|p| p.manifest.id != "linked-plugin"));
        }

        let found = discover_plugin_dirs(dir).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].manifest.id, "dup-plugin");
        assert!(found[0].dir.starts_with(dir));
    }

    #[test]
    fn allowed_permissions_constants() {
        assert_eq!(
            ALLOWED_PERMISSIONS,
            &["read:resources", "read:logs", "read:contexts"]
        );
    }
}
