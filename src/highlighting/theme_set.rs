use super::theme::Theme;
use super::settings::*;
use super::super::LoadingError;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::io::{BufReader, BufRead, Seek};
use walkdir::WalkDir;
use std::fs::File;

#[derive(Debug, Serialize, Deserialize)]
pub struct ThemeSet {
    pub themes: BTreeMap<String, Theme>,
}

/// A set of themes, includes convenient methods for loading and discovering themes.
impl ThemeSet {
    /// Returns all the themes found in a folder, good for enumerating before loading one with get_theme
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
        let file = File::open(path)?;
        let mut file = BufReader::new(file);
        Self::load_from_reader(&mut file)
    }

    /// Loads a theme given a readable stream
    pub fn load_from_reader<R: BufRead + Seek>(r: &mut R) -> Result<Theme, LoadingError> {
        Ok(Theme::parse_settings(read_plist(r)?)?)
    }

    /// Loads all the themes in a folder
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<ThemeSet, LoadingError> {
        let paths = Self::discover_theme_paths(folder)?;
        let mut map = BTreeMap::new();
        for p in &paths {
            let theme = Self::get_theme(p)?;
            let basename =
                p.file_stem().and_then(|x| x.to_str()).ok_or(LoadingError::BadPath)?;
            map.insert(basename.to_owned(), theme);
        }
        Ok(ThemeSet { themes: map })
    }
}


#[cfg(test)]
mod tests {
    use highlighting::{ThemeSet, Color};
    #[test]
    fn can_parse_common_themes() {
        let themes = ThemeSet::load_from_folder("testdata").unwrap();
        let all_themes: Vec<&str> = themes.themes.keys().map(|x| &**x).collect();
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
        // assert!(false);
    }
}
