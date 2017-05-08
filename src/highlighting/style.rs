// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/style.rs
// released under the MIT license by @defuz

/// The foreground, background and font style
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    /// Foreground color.
    pub foreground: Color,
    /// Background color.
    pub background: Color,
    /// Style of the font.
    pub font_style: FontStyle,
}

/// A change to a `Style` applied incrementally by a theme rule.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleModifier {
    /// Foreground color.
    pub foreground: Option<Color>,
    /// Background color.
    pub background: Option<Color>,
    /// Style of the font.
    pub font_style: Option<FontStyle>,
}


/// Pre-defined convenience colour
pub const BLACK: Color = Color {
    r: 0x00,
    g: 0x00,
    b: 0x00,
    a: 0xFF,
};

/// Pre-defined convenience colour
pub const WHITE: Color = Color {
    r: 0xFF,
    g: 0xFF,
    b: 0xFF,
    a: 0xFF,
};

/// RGBA colour, these numbers come directly from the theme so
/// for now you might have to do your own colour space conversion if you are outputting
/// a different colour space from the theme. This can be a problem because some Sublime
/// themes use sRGB and some don't. This is specified in an attribute syntect doesn't parse yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    /// Red component
    pub r: u8,
    /// Green component
    pub g: u8,
    /// Blue component
    pub b: u8,
    /// Alpha component
    pub a: u8,
}

bitflags! {
/// This can be a combination of `FONT_STYLE_BOLD`, `FONT_STYLE_UNDERLINE` and `FONT_STYLE_ITALIC`
    #[derive(Serialize, Deserialize)]
    pub flags FontStyle: u8 {
/// A bitfield constant FontStyle
        const FONT_STYLE_BOLD = 1,
/// A bitfield constant FontStyle
        const FONT_STYLE_UNDERLINE = 2,
/// A bitfield constant FontStyle
        const FONT_STYLE_ITALIC = 4,
    }
}

impl Style {
    /// Applies a change to this style, yielding a new changed style
    pub fn apply(&self, modifier: StyleModifier) -> Style {
        Style {
            foreground: modifier.foreground.unwrap_or(self.foreground),
            background: modifier.background.unwrap_or(self.background),
            font_style: modifier.font_style.unwrap_or(self.font_style),
        }
    }
}
