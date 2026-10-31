//! App-wide UI preferences, stored separately from paired-host metadata and keys.
use anyhow::{Context, Result};
use directories::ProjectDirs;
use homecloud_client::private_write;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct Preferences {
    pub reduced_motion: bool,
}

pub(super) struct LoadedPreferences {
    pub preferences: Preferences,
    pub needs_save: bool,
}

pub(super) fn default_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "", "homecloud")
        .context("Cannot determine the user configuration directory")?;
    Ok(dirs.config_dir().join("preferences.json"))
}

fn legacy_path(profile: &Path) -> PathBuf {
    let mut name = profile.as_os_str().to_owned();
    name.push(".preferences.json");
    Path::new(".").join(PathBuf::from(name))
}

fn read(path: &Path) -> Result<Option<Preferences>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot read the preferences file"),
    };
    serde_json::from_slice(&bytes)
        .context("Invalid preferences file")
        .map(Some)
}

impl Preferences {
    pub fn load(path: &Path, profile: &Path) -> Result<LoadedPreferences> {
        if let Some(preferences) = read(path)? {
            return Ok(LoadedPreferences {
                preferences,
                needs_save: false,
            });
        }
        Ok(LoadedPreferences {
            preferences: read(&legacy_path(profile))?.unwrap_or_default(),
            // Persist defaults too: later profile changes must not import a
            // different legacy setting after app-wide preferences are initialized.
            needs_save: true,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        private_write(path, &serde_json::to_vec_pretty(self)?)
            .context("Cannot write the preferences file")
    }
}
