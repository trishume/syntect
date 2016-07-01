/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/theme.rs
/// released under the MIT license by @defuz

use std::str::FromStr;

use super::settings::{ParseSettings, Settings};
use super::style::*;
use super::selector::*;
use parsing::ParseScopeError;

use self::ParseThemeError::*;

/// A theme parsed from a `.tmTheme` file.
/// Contains fields useful for a theme list as well as `settings` for styling your editor.
#[derive(Debug, Default, RustcEncodable, RustcDecodable)]
pub struct Theme {
    pub name: Option<String>,
    pub author: Option<String>,
    pub settings: ThemeSettings,
    pub scopes: Vec<ThemeItem>,
}

/// Various properties meant to be used to style a text editor.
/// Basically all the styles that aren't directly applied to text like selection colour.
/// Use this to make your editor UI match the highlighted text.
#[derive(Debug, Default, RustcEncodable, RustcDecodable)]
pub struct ThemeSettings {
    /// Foreground color for the view.
    pub foreground: Option<Color>,
    /// Backgound color of the view.
    pub background: Option<Color>,
    /// Color of the caret.
    pub caret: Option<Color>,
    /// Color of the line the caret is in.
    /// Only used when the `higlight_line` setting is set to `true`.
    pub line_highlight: Option<Color>,

    /// Color of bracketed sections of text when the caret is in a bracketed section.
    /// Only applied when the `match_brackets` setting is set to `true`.
    pub bracket_contents_foreground: Option<Color>,
    /// Controls certain options when the caret is in a bracket section.
    /// Only applied when the `match_brackets` setting is set to `true`.
    pub bracket_contents_options: Option<UnderlineOption>,
    /// Foreground color of the brackets when the caret is next to a bracket.
    /// Only applied when the `match_brackets` setting is set to `true`.
    pub brackets_foreground: Option<Color>,
    /// Background color of the brackets when the caret is next to a bracket.
    /// Only applied when the `match_brackets` setting is set to `true`.
    pub brackets_background: Option<Color>,
    /// Controls certain options when the caret is next to a bracket.
    /// Only applied when the match_brackets setting is set to `true`.
    pub brackets_options: Option<UnderlineOption>,

    /// Color of tags when the caret is next to a tag.
    /// Only used when the `match_tags` setting is set to `true`.
    pub tags_foreground: Option<Color>,
    /// Controls certain options when the caret is next to a tag.
    /// Only applied when the match_tags setting is set to `true`.
    pub tags_options: Option<UnderlineOption>,

    /// Background color of regions matching the current search.
    pub find_highlight: Option<Color>,
    /// Background color of regions matching the current search.
    pub find_highlight_foreground: Option<Color>,

    /// Background color of the gutter.
    pub gutter: Option<Color>,
    /// Foreground color of the gutter.
    pub gutter_foreground: Option<Color>,

    /// Color of the selection regions.
    pub selection: Option<Color>,
    /// Background color of the selection regions.
    pub selection_background: Option<Color>,
    /// Color of the selection regions border.
    pub selection_border: Option<Color>,
    /// Color of inactive selections (inactive view).
    pub inactive_selection: Option<Color>,

    /// Color of the guides displayed to indicate nesting levels.
    pub guide: Option<Color>,
    /// Color of the guide lined up with the caret.
    /// Only applied if the `indent_guide_options` setting is set to `draw_active`.
    pub active_guide: Option<Color>,
    /// Color of the current guide’s parent guide level.
    /// Only used if the `indent_guide_options` setting is set to `draw_active`.
    pub stack_guide: Option<Color>,

    /// Background color for regions added via `sublime.add_regions()`
    /// with the `sublime.DRAW_OUTLINED` flag added.
    pub highlight: Option<Color>,
    /// Foreground color for regions added via `sublime.add_regions()`
    /// with the `sublime.DRAW_OUTLINED` flag added.
    pub highlight_foreground: Option<Color>,
}

/// A component of a theme meant to highlight a specific thing (e.g string literals)
/// in a certain way.
#[derive(Debug, Default, RustcEncodable, RustcDecodable)]
pub struct ThemeItem {
    /// Target scope name.
    pub scope: ScopeSelectors,
    pub style: StyleModifier,
}

#[derive(Debug, RustcEncodable, RustcDecodable)]
pub enum UnderlineOption {
    None,
    Underline,
    StippledUnderline,
    SquigglyUnderline,
}

#[derive(Debug)]
pub enum ParseThemeError {
    IncorrectUnderlineOption,
    IncorrectFontStyle(String),
    IncorrectColor,
    IncorrectSyntax,
    IncorrectSettings,
    UndefinedSettings,
    UndefinedScopeSettings(String),
    ColorShemeScopeIsNotObject,
    ColorShemeSettingsIsNotObject,
    ScopeSelectorIsNotString(String),
    DuplicateSettings,
    ScopeParse(ParseScopeError),
}

impl From<ParseScopeError> for ParseThemeError {
    fn from(error: ParseScopeError) -> ParseThemeError {
        ScopeParse(error)
    }
}

impl Default for UnderlineOption {
    fn default() -> UnderlineOption {
        UnderlineOption::None
    }
}

impl Default for FontStyle {
    fn default() -> FontStyle {
        FontStyle::empty()
    }
}

impl FromStr for UnderlineOption {
    type Err = ParseThemeError;

    fn from_str(s: &str) -> Result<UnderlineOption, Self::Err> {
        Ok(match s {
            "underline" => UnderlineOption::Underline,
            "stippled_underline" => UnderlineOption::StippledUnderline,
            "squiggly_underline" => UnderlineOption::SquigglyUnderline,
            _ => return Err(IncorrectUnderlineOption),
        })
    }
}

impl ParseSettings for UnderlineOption {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<UnderlineOption, Self::Error> {
        match settings {
            Settings::String(value) => Ok(try!(UnderlineOption::from_str(&value))),
            _ => Err(IncorrectUnderlineOption),
        }
    }
}

impl FromStr for FontStyle {
    type Err = ParseThemeError;

    fn from_str(s: &str) -> Result<FontStyle, Self::Err> {
        let mut font_style = FontStyle::empty();
        for i in s.split_whitespace() {
            font_style.insert(match i {
                "bold" => FONT_STYLE_BOLD,
                "underline" => FONT_STYLE_UNDERLINE,
                "italic" => FONT_STYLE_ITALIC,
                "normal" => FontStyle::empty(),
                "regular" => FontStyle::empty(),
                s => return Err(IncorrectFontStyle(s.to_owned())),
            })
        }
        Ok(font_style)
    }
}

impl ParseSettings for FontStyle {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<FontStyle, Self::Error> {
        match settings {
            Settings::String(value) => Ok(try!(FontStyle::from_str(&value))),
            c => Err(IncorrectFontStyle(c.to_string())),
        }
    }
}

impl FromStr for Color {
    type Err = ParseThemeError;

    fn from_str(s: &str) -> Result<Color, Self::Err> {
        let mut chars = s.chars();
        if chars.next() != Some('#') {
            return Err(IncorrectColor);
        }
        let mut d = Vec::new();
        for char in chars {
            d.push(try!(char.to_digit(16).ok_or(IncorrectColor)) as u8);
        }
        Ok(match d.len() {
            3 => {
                Color {
                    r: d[0],
                    g: d[1],
                    b: d[2],
                    a: 255,
                }
            }
            6 => {
                Color {
                    r: d[0] * 16 + d[1],
                    g: d[2] * 16 + d[3],
                    b: d[4] * 16 + d[5],
                    a: 255,
                }
            }
            8 => {
                Color {
                    r: d[0] * 16 + d[1],
                    g: d[2] * 16 + d[3],
                    b: d[4] * 16 + d[5],
                    a: d[6] * 16 + d[7],
                }
            }
            _ => return Err(IncorrectColor),
        })
    }
}

impl ParseSettings for Color {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<Color, Self::Error> {
        match settings {
            Settings::String(value) => Ok(try!(Color::from_str(&value))),
            _ => Err(IncorrectColor),
        }
    }
}

impl ParseSettings for StyleModifier {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<StyleModifier, Self::Error> {
        let mut obj = match settings {
            Settings::Object(obj) => obj,
            _ => return Err(ColorShemeScopeIsNotObject),
        };
        let font_style = match obj.remove("fontStyle") {
            Some(Settings::String(value)) => Some(try!(FontStyle::from_str(&value))),
            None => None,
            Some(c) => return Err(IncorrectFontStyle(c.to_string())),
        };
        let foreground = match obj.remove("foreground") {
            Some(Settings::String(value)) => Some(try!(Color::from_str(&value))),
            None => None,
            _ => return Err(IncorrectColor),
        };
        let background = match obj.remove("background") {
            Some(Settings::String(value)) => Some(try!(Color::from_str(&value))),
            None => None,
            _ => return Err(IncorrectColor),
        };

        Ok(StyleModifier {
            foreground: foreground,
            background: background,
            font_style: font_style,
        })
    }
}

impl ParseSettings for ThemeItem {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<ThemeItem, Self::Error> {
        let mut obj = match settings {
            Settings::Object(obj) => obj,
            _ => return Err(ColorShemeScopeIsNotObject),
        };
        let scope = match obj.remove("scope") {
            Some(Settings::String(value)) => try!(ScopeSelectors::from_str(&value)),
            _ => return Err(ScopeSelectorIsNotString(format!("{:?}", obj))),
        };
        let style = match obj.remove("settings") {
            Some(settings) => try!(StyleModifier::parse_settings(settings)),
            None => return Err(IncorrectSettings),
        };
        Ok(ThemeItem {
            scope: scope,
            style: style,
        })
    }
}

impl ParseSettings for ThemeSettings {
    type Error = ParseThemeError;

    fn parse_settings(json: Settings) -> Result<ThemeSettings, Self::Error> {
        let mut settings = ThemeSettings::default();

        let obj = match json {
            Settings::Object(obj) => obj,
            _ => return Err(ColorShemeSettingsIsNotObject),
        };

        for (key, value) in obj {
            match &key[..] {
                "foreground" => settings.foreground = Color::parse_settings(value).ok(),
                "background" => settings.background = Color::parse_settings(value).ok(),
                "caret" => settings.caret = Color::parse_settings(value).ok(),
                "lineHighlight" => settings.line_highlight = Color::parse_settings(value).ok(),
                "bracketContentsForeground" => {
                    settings.bracket_contents_foreground = Color::parse_settings(value).ok()
                }
                "bracketContentsOptions" => {
                    settings.bracket_contents_options = UnderlineOption::parse_settings(value).ok()
                }
                "bracketsForeground" => {
                    settings.brackets_foreground = Color::parse_settings(value).ok()
                }
                "bracketsBackground" => {
                    settings.brackets_background = Color::parse_settings(value).ok()
                }
                "bracketsOptions" => {
                    settings.brackets_options = UnderlineOption::parse_settings(value).ok()
                }
                "tagsForeground" => settings.tags_foreground = Color::parse_settings(value).ok(),
                "tagsOptions" => {
                    settings.tags_options = UnderlineOption::parse_settings(value).ok()
                }
                "findHighlight" => settings.find_highlight = Color::parse_settings(value).ok(),
                "findHighlightForeground" => {
                    settings.find_highlight_foreground = Color::parse_settings(value).ok()
                }
                "gutter" => settings.gutter = Color::parse_settings(value).ok(),
                "gutterForeground" => {
                    settings.gutter_foreground = Color::parse_settings(value).ok()
                }
                "selection" => settings.selection = Color::parse_settings(value).ok(),
                "selectionBackground" => {
                    settings.selection_background = Color::parse_settings(value).ok()
                }
                "selectionBorder" => settings.selection_border = Color::parse_settings(value).ok(),
                "inactiveSelection" => {
                    settings.inactive_selection = Color::parse_settings(value).ok()
                }
                "guide" => settings.guide = Color::parse_settings(value).ok(),
                "activeGuide" => settings.active_guide = Color::parse_settings(value).ok(),
                "stackGuide" => settings.stack_guide = Color::parse_settings(value).ok(),
                "highlight" => settings.highlight = Color::parse_settings(value).ok(),
                "highlightForeground" => {
                    settings.highlight_foreground = Color::parse_settings(value).ok()
                }
                "invisibles" => (), // ignored
                _ => (),
            }
        }
        Ok(settings)
    }
}

impl ParseSettings for Theme {
    type Error = ParseThemeError;

    fn parse_settings(settings: Settings) -> Result<Theme, Self::Error> {
        let mut obj = match settings {
            Settings::Object(obj) => obj,
            _ => return Err(IncorrectSyntax),
        };
        let name = match obj.remove("name") {
            Some(Settings::String(name)) => Some(name),
            None => None,
            _ => return Err(IncorrectSyntax),
        };
        let author = match obj.remove("author") {
            Some(Settings::String(author)) => Some(author),
            None => None,
            _ => return Err(IncorrectSyntax),
        };
        let items = match obj.remove("settings") {
            Some(Settings::Array(items)) => items,
            _ => return Err(IncorrectSyntax),
        };
        let mut iter = items.into_iter();
        let settings = match iter.next() {
            Some(Settings::Object(mut obj)) => {
                match obj.remove("settings") {
                    Some(settings) => try!(ThemeSettings::parse_settings(settings)),
                    None => return Err(UndefinedSettings),
                }
            }
            _ => return Err(UndefinedSettings),
        };
        let mut scopes = Vec::new();
        for json in iter {
            // TODO option to disable best effort parsing and bubble up warnings
            if let Ok(item) = ThemeItem::parse_settings(json) {
                scopes.push(item);
            }
        }
        Ok(Theme {
            name: name,
            author: author,
            settings: settings,
            scopes: scopes,
        })
    }
}
