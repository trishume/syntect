//! Everything having to do with turning parsed text into styled text.
//!
//! You might want to check out [`Theme`] for its handy text-editor related settings like selection
//! color, [`ThemeSet`] for loading themes, as well as things starting with `Highlight` for how to
//! highlight text.
//!
//! [`Theme`]: struct.Theme.html
//! [`ThemeSet`]: struct.ThemeSet.html
mod highlighter;
mod selector;
pub(crate) mod settings;
mod style;
mod theme;
mod theme_set;

pub use self::highlighter::*;
pub use self::selector::*;
pub use self::settings::SettingsError;
pub use self::style::*;
pub use self::theme::*;
pub use self::theme_set::*;
