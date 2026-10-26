//! Settings kept between runs of the browser, in a file of `name value`
//! lines: `~/.config/netscheme/settings`, or under `$XDG_CONFIG_HOME`.

use std::{collections::BTreeMap, fs, path::PathBuf};

pub struct Settings {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Settings {
    /// The settings in the user's config folder, if there's a home to find it in.
    pub fn standard() -> Option<Settings> {
        let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(Settings::at(config.join("netscheme").join("settings")))
    }

    /// The settings in the file at `path`. A missing or unreadable file is no settings.
    pub fn at(path: PathBuf) -> Settings {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let values =
            text.lines().filter_map(|line| line.trim().split_once(' ')).map(|(name, value)| (name.into(), value.trim().into())).collect();
        Settings { path, values }
    }

    /// Whether dark mode was chosen, if it ever was.
    pub fn dark_mode(&self) -> Option<bool> {
        self.values.get("dark-mode").and_then(|value| value.parse().ok())
    }

    pub fn set_dark_mode(&mut self, dark: bool) {
        self.values.insert("dark-mode".into(), dark.to_string());
        self.save();
    }

    fn save(&self) {
        let text: String = self.values.iter().map(|(name, value)| format!("{name} {value}\n")).collect();
        let saved = self.path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&self.path, text));
        if let Err(e) = saved {
            eprintln!("couldn't save settings to {}: {e}", self.path.display());
        }
    }
}
