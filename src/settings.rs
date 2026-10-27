//! Settings kept between runs of the browser, in a file of `name value`
//! lines: `~/.config/netscheme/settings`, or under `$XDG_CONFIG_HOME`.
//! Bookmarks are kept beside it, in `bookmarks`, one `location<tab>title`
//! line each.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub struct Settings {
    path: PathBuf,
    values: BTreeMap<String, String>,
    bookmarks: Vec<Bookmark>,
}

/// A site the user wants to come back to.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub location: String,
    pub title: String,
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

    /// The settings in the file at `path`, and the bookmarks beside it. A
    /// missing or unreadable file is no settings, or no bookmarks.
    pub fn at(path: PathBuf) -> Settings {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let values =
            text.lines().filter_map(|line| line.trim().split_once(' ')).map(|(name, value)| (name.into(), value.trim().into())).collect();
        let text = fs::read_to_string(path.with_file_name("bookmarks")).unwrap_or_default();
        let bookmarks = text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(location, title)| Bookmark { location: location.into(), title: title.into() })
            .collect();
        Settings { path, values, bookmarks }
    }

    /// Whether dark mode was chosen, if it ever was.
    pub fn dark_mode(&self) -> Option<bool> {
        self.values.get("dark-mode").and_then(|value| value.parse().ok())
    }

    pub fn set_dark_mode(&mut self, dark: bool) {
        self.values.insert("dark-mode".into(), dark.to_string());
        self.save();
    }

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    pub fn set_bookmarks(&mut self, bookmarks: Vec<Bookmark>) {
        let text: String = bookmarks.iter().map(|b| format!("{}\t{}\n", b.location, b.title)).collect();
        write(&self.path.with_file_name("bookmarks"), text);
        self.bookmarks = bookmarks;
    }

    fn save(&self) {
        let text: String = self.values.iter().map(|(name, value)| format!("{name} {value}\n")).collect();
        write(&self.path, text);
    }
}

fn write(path: &Path, text: String) {
    let saved = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(path, text));
    if let Err(e) = saved {
        eprintln!("couldn't save {}: {e}", path.display());
    }
}
