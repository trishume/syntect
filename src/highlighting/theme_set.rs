use super::theme::Theme;
use super::settings::*;
use super::super::LoadingError;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ThemeSet {
    // This is a `BTreeMap` because they're faster than hashmaps on small sets
    pub themes: BTreeMap<String, Theme>,
}

/// A set of themes, includes convenient methods for loading and discovering themes.
impl ThemeSet {
    /// Creates an empty set
    pub fn new() -> ThemeSet {
        ThemeSet::default()
    }

    /// Returns all the themes found in a folder
    ///
    /// This is god for enumerating before loading one with [`get_theme`](#method.get_theme)
    pub fn discover_theme_paths<P: AsRef<Path>>(folder: P) -> Result<Vec<PathBuf>, LoadingError> {
        let mut themes = Vec::new();
        for entry in WalkDir::new(folder) {
            let entry = entry.map_err(LoadingError::WalkDir)?;
            if entry.path().extension().map_or(false, |e| e == "tmTheme") {
                themes.push(entry.path().to_owned());
            }
        }
        Ok(themes)
    }

    /// Loads a theme given a path to a .tmTheme file
    pub fn get_theme<P: AsRef<Path>>(path: P) -> Result<Theme, LoadingError> {
        let file = std::fs::File::open(path)?;
        let mut file = std::io::BufReader::new(file);
        Self::load_from_reader(&mut file)
    }

    /// Loads a theme given a readable stream
    pub fn load_from_reader<R: std::io::BufRead + std::io::Seek>(r: &mut R) -> Result<Theme, LoadingError> {
        Ok(Theme::parse_settings(read_plist(r)?)?)
    }

    /// Generate a `ThemeSet` from all themes in a folder
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<ThemeSet, LoadingError> {
        let mut theme_set = Self::new();
        theme_set.add_from_folder(folder)?;
        Ok(theme_set)
    }

    /// Load all the themes in the folder into this `ThemeSet`
    pub fn add_from_folder<P: AsRef<Path>>(&mut self, folder: P) -> Result<(), LoadingError> {
        let paths = Self::discover_theme_paths(folder)?;
        for p in &paths {
            let theme = Self::get_theme(p)?;
            let basename =
                p.file_stem().and_then(|x| x.to_str()).ok_or(LoadingError::BadPath)?;
            self.themes.insert(basename.to_owned(), theme);
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use crate::highlighting::{ThemeSet, Color};
    #[test]
    fn can_parse_common_themes() {
        let themes = ThemeSet::load_from_folder("testdata").unwrap();
        let all_themes: Vec<&str> = themes.themes.keys().map(|x| &**x).collect();
        assert!(all_themes.contains(&"base16-ocean.dark"));

        println!("{:?}", all_themes);

        let theme = ThemeSet::get_theme("testdata/spacegray/base16-ocean.dark.tmTheme").unwrap();
        assert_eq!(theme.name.unwrap(), "Base16 Ocean Dark");
        assert_eq!(theme.settings.selection.unwrap(),
                   Color {
                       r: 0x4f,
                       g: 0x5b,
                       b: 0x66,
                       a: 0xff,
                   });
        assert_eq!(theme.scopes[0].style.foreground.unwrap(),
                   Color {
                       r: 0xc0,
                       g: 0xc5,
                       b: 0xce,
                       a: 0xFF,
                   });
        // unreachable!();
    }
}
