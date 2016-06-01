/// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/style.rs
/// released under the MIT license by @defuz

#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// Foreground color.
    pub foreground: Color,
    /// Background color.
    pub background: Color,
    /// Style of the font.
    pub font_style: FontStyle
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StyleModifier {
    /// Foreground color.
    pub foreground: Option<Color>,
    /// Background color.
    pub background: Option<Color>,
    /// Style of the font.
    pub font_style: Option<FontStyle>
}

pub const BLACK: Color = Color {r: 0x00, g: 0x00, b: 0x00, a: 0x00};
pub const WHITE: Color = Color {r: 0xFF, g: 0xFF, b: 0xFF, a: 0xFF};

#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8
}

bitflags! {
    flags FontStyle: u8 {
        const FONT_STYLE_BOLD = 1,
        const FONT_STYLE_UNDERLINE = 2,
        const FONT_STYLE_ITALIC = 4,
    }
}

impl Style {
    pub fn apply(&self, modifier: StyleModifier) -> Style {
        Style {
            foreground: modifier.foreground.unwrap_or(self.foreground),
            background: modifier.background.unwrap_or(self.background),
            font_style: modifier.font_style.unwrap_or(self.font_style),
        }
    }
}
