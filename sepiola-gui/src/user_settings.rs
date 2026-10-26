use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_RECENT_LIBRARIES: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    pub input_dir: String,
    pub output_dir: String,
    pub backend: String,
    pub classifier: String,
    pub workers: usize,
    pub jpeg_quality: u8,
    pub max_clipped_high: f32,
    pub model_path: String,
    pub portrait_lut: String,
    pub portrait_strength: f32,
    pub other_lut: String,
    pub other_strength: f32,
    pub pixel_format: String,
    pub keep_metadata: bool,
    pub identity_enabled: bool,
    pub identity_model: String,
    pub landmark_model: String,
    pub identity_threshold: f32,
    pub gallery_file: String,
    pub library_root: String,
    pub recent_library_roots: Vec<String>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            input_dir: String::new(),
            output_dir: String::new(),
            backend: "auto".to_string(),
            classifier: "face".to_string(),
            workers: 4,
            jpeg_quality: 92,
            max_clipped_high: 0.02,
            model_path: String::new(),
            portrait_lut: String::new(),
            portrait_strength: 0.4,
            other_lut: String::new(),
            other_strength: 0.7,
            pixel_format: "8bit".to_string(),
            keep_metadata: true,
            identity_enabled: true,
            identity_model: String::new(),
            landmark_model: String::new(),
            identity_threshold: 0.35,
            gallery_file: "identities.json".to_string(),
            library_root: String::new(),
            recent_library_roots: Vec::new(),
        }
    }
}

impl UserSettings {
    pub fn load_or_default() -> Self {
        match settings_path() {
            Ok(path) => Self::load_or_default_from(&path),
            Err(error) => {
                eprintln!("Unable to load user settings: {error}");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = settings_path()?;
        self.save_to(&path)
    }

    pub fn remember_library(&mut self, path: &Path) {
        let root = path.to_string_lossy().trim().to_string();
        if root.is_empty() {
            return;
        }
        self.library_root = root.clone();
        self.recent_library_roots
            .retain(|existing| existing != &root);
        self.recent_library_roots.insert(0, root);
        self.recent_library_roots.truncate(MAX_RECENT_LIBRARIES);
    }

    pub fn record_active_library_if_changed(&mut self, previous_root: &str) {
        if self.library_root != previous_root && !self.library_root.trim().is_empty() {
            let root = PathBuf::from(&self.library_root);
            self.remember_library(&root);
        }
    }

    fn load_from(path: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        let mut settings: Self = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        settings.normalize_recent_libraries();
        Ok(settings)
    }

    fn load_or_default_from(path: &Path) -> Self {
        match Self::load_from(path) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("Unable to load user settings: {error}");
                Self::default()
            }
        }
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
        })?;
        fs::create_dir_all(parent)?;
        let text = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = parent.join(format!(".user-settings-{}.tmp", std::process::id()));
        fs::write(&temporary, text)?;
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) if path.exists() => {
                fs::remove_file(path)?;
                fs::rename(&temporary, path).map_err(|_| error)
            }
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(error)
            }
        }
    }

    fn normalize_recent_libraries(&mut self) {
        let recent = std::mem::take(&mut self.recent_library_roots);
        let mut seen = HashSet::new();
        self.recent_library_roots = recent
            .into_iter()
            .filter_map(|root| {
                let root = root.trim().to_string();
                (!root.is_empty() && seen.insert(root.clone())).then_some(root)
            })
            .take(MAX_RECENT_LIBRARIES)
            .collect();
        if !self.library_root.trim().is_empty() {
            let active = self.library_root.clone();
            self.recent_library_roots.retain(|root| root != &active);
            self.recent_library_roots.insert(0, active);
            self.recent_library_roots.truncate(MAX_RECENT_LIBRARIES);
        }
    }
}

fn settings_path() -> io::Result<PathBuf> {
    let config_dir = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "user config directory is unavailable",
        )
    })?;

    Ok(config_dir.join("sepiola").join("user-settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir()
            .join(format!("sepiola-user-settings-{tag}-{nanos}"))
            .join("user-settings.json")
    }

    #[test]
    fn settings_round_trip() {
        let path = temp_path("round-trip");
        let mut settings = UserSettings {
            workers: 7,
            jpeg_quality: 88,
            ..UserSettings::default()
        };
        settings.remember_library(Path::new("/books/one"));
        settings.save_to(&path).expect("save settings");
        assert_eq!(
            UserSettings::load_from(&path).expect("load settings"),
            settings
        );
        let _ = fs::remove_dir_all(path.parent().expect("temp dir"));
    }

    #[test]
    fn recent_libraries_are_unique_bounded_and_most_recent_first() {
        let mut settings = UserSettings::default();
        for number in 0..10 {
            settings.remember_library(&PathBuf::from(format!("/books/{number}")));
        }
        settings.remember_library(Path::new("/books/5"));

        assert_eq!(settings.recent_library_roots.len(), MAX_RECENT_LIBRARIES);
        assert_eq!(settings.recent_library_roots[0], "/books/5");
        assert!(!settings
            .recent_library_roots
            .contains(&"/books/0".to_string()));
        assert_eq!(
            settings
                .recent_library_roots
                .iter()
                .filter(|root| *root == "/books/5")
                .count(),
            1
        );
    }

    #[test]
    fn changed_active_library_updates_the_snapshot_and_recent_list() {
        let previous = UserSettings::default();
        let mut current = previous.clone();
        current.library_root = "/books/one".to_string();

        current.record_active_library_if_changed(&previous.library_root);

        assert_ne!(current, previous);
        assert_eq!(current.library_root, "/books/one");
        assert_eq!(current.recent_library_roots, ["/books/one"]);

        let path = temp_path("active-library");
        current.save_to(&path).expect("save active library");
        assert_eq!(
            UserSettings::load_from(&path).expect("load active library"),
            current
        );
        let _ = fs::remove_dir_all(path.parent().expect("temp dir"));
    }

    #[test]
    fn missing_settings_use_defaults_and_corrupt_settings_are_rejected() {
        let path = temp_path("invalid");
        assert_eq!(
            UserSettings::load_from(&path).expect("missing settings"),
            UserSettings::default()
        );
        fs::create_dir_all(path.parent().expect("temp dir")).expect("create temp dir");
        fs::write(&path, b"not-json").expect("write invalid settings");
        assert_eq!(
            UserSettings::load_from(&path)
                .expect_err("invalid settings should fail")
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            UserSettings::load_or_default_from(&path),
            UserSettings::default()
        );
        let _ = fs::remove_dir_all(path.parent().expect("temp dir"));
    }
}
