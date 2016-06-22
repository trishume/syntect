//! Everything having to do with turning parsed text into styled text.
//! You might want to check out `Theme` for its handy text-editor related
//! settings like selection colour, `ThemeSet` for loading themes,
//! as well as things starting with `Highlight` for how to highlight text.
mod selector;
mod settings;
mod style;
mod theme;
mod highlighter;
mod theme_set;

pub use self::selector::*;
pub use self::settings::SettingsError;
pub use self::style::*;
pub use self::theme::*;
pub use self::highlighter::*;
pub use self::theme_set::*;
