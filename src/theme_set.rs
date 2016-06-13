use theme::theme::{Theme, ParseThemeError};
use theme::settings::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::io::{Error as IoError, BufReader};
use walkdir::WalkDir;
use std::io::{self};
use std::fs::File;
use walkdir;

#[derive(Debug, RustcEncodable, RustcDecodable)]
pub struct ThemeSet {
  themes: BTreeMap<String, Theme>,
}

#[derive(Debug)]
pub enum ThemeSetError {
    WalkDir(walkdir::Error),
    Io(io::Error),
    ParseTheme(ParseThemeError),
    ReadSettings(SettingsError),
    BadPath,
}

impl From<SettingsError> for ThemeSetError {
    fn from(error: SettingsError) -> ThemeSetError {
        ThemeSetError::ReadSettings(error)
    }
}

impl From<IoError> for ThemeSetError {
    fn from(error: IoError) -> ThemeSetError {
        ThemeSetError::Io(error)
    }
}

impl From<ParseThemeError> for ThemeSetError {
    fn from(error: ParseThemeError) -> ThemeSetError {
        ThemeSetError::ParseTheme(error)
    }
}

impl ThemeSet {
    /// Returns all the themes found in a folder, good for enumerating before loading one with get_theme
    pub fn discover_theme_paths<P: AsRef<Path>>(folder: P) -> Result<Vec<PathBuf>, ThemeSetError> {
      let mut themes = Vec::new();
      for entry in WalkDir::new(folder) {
          let entry = try!(entry.map_err(|e| ThemeSetError::WalkDir(e)));
          if entry.path().extension().map(|e| e == "tmTheme").unwrap_or(false) {
              themes.push(entry.path().to_owned());
          }
      }
      Ok(themes)
    }

    fn read_file(path: &Path) -> Result<BufReader<File>, ThemeSetError> {
      let reader = try!(File::open(path));
      Ok(BufReader::new(reader))
    }

    fn read_plist(path: &Path) -> Result<Settings, ThemeSetError> {
      Ok(try!(read_plist(try!(Self::read_file(path)))))
    }

    /// Loads a theme given a path to a .tmTheme file
    pub fn get_theme<P: AsRef<Path>>(path: P) -> Result<Theme, ThemeSetError> {
      Ok(try!(Theme::parse_settings(try!(Self::read_plist(path.as_ref())))))
    }

    /// Loads all the themes in a folder
    pub fn load_from_folder<P: AsRef<Path>>(folder: P) -> Result<ThemeSet, ThemeSetError> {
        let paths = try!(Self::discover_theme_paths(folder));
        let mut map = BTreeMap::new();
        for p in paths.iter() {
            let theme = try!(Self::get_theme(p));
            let basename = try!(p.file_stem().and_then(|x| x.to_str()).ok_or(ThemeSetError::BadPath));
            map.insert(basename.to_owned(), theme);
        }
        Ok(ThemeSet { themes: map })
    }
}


#[cfg(test)]
mod tests {
    use theme_set::ThemeSet;
    #[test]
    fn can_parse_common_themes() {
        use theme::style::Color;
        let themes = ThemeSet::load_from_folder("testdata").unwrap();
        let all_themes: Vec<&str> = themes.themes.keys().map(|x| &**x).collect();
        println!("{:?}", all_themes);

        let theme = ThemeSet::get_theme("testdata/themes.tmbundle/Themes/Amy.tmTheme").unwrap();
        assert_eq!(theme.name.unwrap(), "Amy");
        assert_eq!(theme.settings.selection.unwrap(),
                   Color {
                       r: 0x80,
                       g: 0x00,
                       b: 0x00,
                       a: 0x80,
                   });
        assert_eq!(theme.scopes[0].style.foreground.unwrap(),
                   Color {
                       r: 0x40,
                       g: 0x40,
                       b: 0x80,
                       a: 0xFF,
                   });
        // assert!(false);
    }
}
