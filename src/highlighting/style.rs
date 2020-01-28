// Code based on https://github.com/defuz/sublimate/blob/master/src/core/syntax/style.rs
// released under the MIT license by @defuz
use bitflags::bitflags;

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

/// RGBA color, these numbers come directly from the theme so
/// for now you might have to do your own color space conversion if you are outputting
/// a different color space from the theme. This can be a problem because some Sublime
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
    /// This can be a combination of `BOLD`, `UNDERLINE` and `ITALIC`
    #[derive(Serialize, Deserialize)]
    pub struct FontStyle: u8 {
        /// Bold font style
        const BOLD = 1;
        /// Underline font style
        const UNDERLINE = 2;
        /// Italic font style
        const ITALIC = 4;
    }
}


impl Color {
    /// Black color (`#000000`)
    pub const BLACK: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };

    /// White color (`#FFFFFF`)
    pub const WHITE: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };

    /// Red CSS colors /////////////////////////////////////////////////////////
    pub const LIGHTSALMON: Color = Color {
        r: 0xFF,
        g: 0xA0,
        b: 0x7A,
        a: 0xFF,
    };
    pub const SALMON: Color = Color {
        r: 0xFA,
        g: 0x80,
        b: 0x72,
        a: 0xFF,
    };
    pub const DARKSALMON: Color = Color {
        r: 0xE9,
        g: 0x96,
        b: 0x7A,
        a: 0xFF,
    };
    pub const LIGHTCORAL: Color = Color {
        r: 0xF0,
        g: 0x80,
        b: 0x80,
        a: 0xFF,
    };
    pub const INDIANRED: Color = Color {
        r: 0xCD,
        g: 0x5C,
        b: 0x5C,
        a: 0xFF,
    };
    pub const CRIMSON: Color = Color {
        r: 0xDC,
        g: 0x14,
        b: 0x3C,
        a: 0xFF,
    };
    pub const FIREBRICK: Color = Color {
        r: 0xB2,
        g: 0x22,
        b: 0x22,
        a: 0xFF,
    };
    pub const RED: Color = Color {
        r: 0xFF,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };
    pub const DARKRED: Color = Color {
        r: 0x8B,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };

    /// Orange CSS colors //////////////////////////////////////////////////////
    pub const CORAL: Color = Color {
        r: 0xFF,
        g: 0x7F,
        b: 0x50,
        a: 0xFF,
    };
    pub const TOMATO: Color = Color {
        r: 0xFF,
        g: 0x63,
        b: 0x47,
        a: 0xFF,
    };
    pub const ORANGERED: Color = Color {
        r: 0xFF,
        g: 0x45,
        b: 0x00,
        a: 0xFF,
    };
    pub const GOLD: Color = Color {
        r: 0xFF,
        g: 0xD7,
        b: 0x00,
        a: 0xFF,
    };
    pub const ORANGE: Color = Color {
        r: 0xFF,
        g: 0xA5,
        b: 0x00,
        a: 0xFF,
    };
    pub const DARKORANGE: Color = Color {
        r: 0xFF,
        g: 0x8C,
        b: 0x00,
        a: 0xFF,
    };

    /// Yellow CSS colors //////////////////////////////////////////////////////
    pub const LIGHTYELLOW: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0xE0,
        a: 0xFF,
    };
    pub const LEMONCHIFFON: Color = Color {
        r: 0xFF,
        g: 0xFA,
        b: 0xCD,
        a: 0xFF,
    };
    pub const LIGHTGOLDENRODYELLOW: Color = Color {
        r: 0xFA,
        g: 0xFA,
        b: 0xD2,
        a: 0xFF,
    };
    pub const PAPAYAWHIP: Color = Color {
        r: 0xFF,
        g: 0xEF,
        b: 0xD5,
        a: 0xFF,
    };
    pub const MOCCASIN: Color = Color {
        r: 0xFF,
        g: 0xE4,
        b: 0xB5,
        a: 0xFF,
    };
    pub const PEACHPUFF: Color = Color {
        r: 0xFF,
        g: 0xDA,
        b: 0xB9,
        a: 0xFF,
    };
    pub const PALEGOLDENROD: Color = Color {
        r: 0xEE,
        g: 0xE8,
        b: 0xAA,
        a: 0xFF,
    };
    pub const KHAKI: Color = Color {
        r: 0xF0,
        g: 0xE6,
        b: 0x8C,
        a: 0xFF,
    };
    pub const DARKKHAKI: Color = Color {
        r: 0xBD,
        g: 0xB7,
        b: 0x6B,
        a: 0xFF,
    };
    pub const YELLOW: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0x00,
        a: 0xFF,
    };


    /// Green CSS colors ///////////////////////////////////////////////////////
    pub const LAWNGREEN: Color = Color {
        r: 0x7C,
        g: 0xFC,
        b: 0x00,
        a: 0xFF,
    };
    pub const CHARTREUSE: Color = Color {
        r: 0x7F,
        g: 0xFF,
        b: 0x00,
        a: 0xFF,
    };
    pub const LIMEGREEN: Color = Color {
        r: 0x32,
        g: 0xCD,
        b: 0x32,
        a: 0xFF,
    };
    pub const LIME: Color = Color {
        r: 0x00,
        g: 0xFF,
        b: 0x00,
        a: 0xFF,
    };
    pub const FORESTGREEN: Color = Color {
        r: 0x22,
        g: 0x8B,
        b: 0x22,
        a: 0xFF,
    };
    pub const GREEN: Color = Color {
        r: 0x00,
        g: 0x80,
        b: 0x00,
        a: 0xFF,
    };
    pub const DARKGREEN: Color = Color {
        r: 0x00,
        g: 0x64,
        b: 0x00,
        a: 0xFF,
    };
    pub const GREENYELLOW: Color = Color {
        r: 0xAD,
        g: 0xFF,
        b: 0x2F,
        a: 0xFF,
    };
    pub const YELLOWGREEN: Color = Color {
        r: 0x9A,
        g: 0xCD,
        b: 0x32,
        a: 0xFF,
    };
    pub const SPRINGGREEN: Color = Color {
        r: 0x00,
        g: 0xFF,
        b: 0x7F,
        a: 0xFF,
    };
    pub const MEDIUMSPINGGREEN: Color = Color {
        r: 0x00,
        g: 0xFA,
        b: 0x9A,
        a: 0xFF,
    };
    pub const LIGHTGREEN: Color = Color {
        r: 0x90,
        g: 0xEE,
        b: 0x90,
        a: 0xFF,
    };
    pub const PALEGREEN: Color = Color {
        r: 0x98,
        g: 0xFB,
        b: 0x98,
        a: 0xFF,
    };
    pub const DARKSEAGREEN: Color = Color {
        r: 0x8F,
        g: 0xBC,
        b: 0x8F,
        a: 0xFF,
    };
    pub const MEDIUMSEAGREEN: Color = Color {
        r: 0x3C,
        g: 0xB3,
        b: 0x71,
        a: 0xFF,
    };
    pub const SEAGREEN: Color = Color {
        r: 0x2E,
        g: 0x8B,
        b: 0x57,
        a: 0xFF,
    };
    pub const OLIVE: Color = Color {
        r: 0x80,
        g: 0x80,
        b: 0x00,
        a: 0xFF,
    };
    pub const DARKOLIVE: Color = Color {
        r: 0x55,
        g: 0x6B,
        b: 0x2F,
        a: 0xFF,
    };
    pub const OLIVEDRAB: Color = Color {
        r: 0x6B,
        g: 0x8E,
        b: 0x23,
        a: 0xFF,
    };

    /// Cyan CSS colors ////////////////////////////////////////////////////////
    pub const LIGHTCYAN: Color = Color {
        r: 0xE0,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const CYAN: Color = Color {
        r: 0x00,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const AQUA: Color = Color {
        r: 0xE0,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const AQUAMARINE: Color = Color {
        r: 0x7F,
        g: 0xFF,
        b: 0xD4,
        a: 0xFF,
    };
    pub const MEDIUMAQUAMARINE: Color = Color {
        r: 0x66,
        g: 0xCD,
        b: 0xAA,
        a: 0xFF,
    };
    pub const PALETURQUOISE: Color = Color {
        r: 0xAF,
        g: 0xEE,
        b: 0xEE,
        a: 0xFF,
    };
    pub const TURQUOISE: Color = Color {
        r: 0x40,
        g: 0xE0,
        b: 0xD0,
        a: 0xFF,
    };
    pub const MEDIUMTURQUOISE: Color = Color {
        r: 0x48,
        g: 0xD1,
        b: 0xCC,
        a: 0xFF,
    };
    pub const DARKTURQUOISE: Color = Color {
        r: 0x00,
        g: 0xCE,
        b: 0xD1,
        a: 0xFF,
    };
    pub const LIGHTSEAGREEN: Color = Color {
        r: 0x20,
        g: 0xB2,
        b: 0xAA,
        a: 0xFF,
    };
    pub const CADETBLUE: Color = Color {
        r: 0x5F,
        g: 0x9E,
        b: 0xA0,
        a: 0xFF,
    };
    pub const DARKCYAN: Color = Color {
        r: 0x00,
        g: 0x8B,
        b: 0x8B,
        a: 0xFF,
    };
    pub const TEAL: Color = Color {
        r: 0x00,
        g: 0x80,
        b: 0x80,
        a: 0xFF,
    };

    /// Blue CSS colors ////////////////////////////////////////////////////////
    pub const POWDERBLUE: Color = Color {
        r: 0xB0,
        g: 0xE0,
        b: 0xE6,
        a: 0xFF,
    };
    pub const LIGHTBLUE: Color = Color {
        r: 0xAD,
        g: 0xD8,
        b: 0xE6,
        a: 0xFF,
    };
    pub const LIGHTSKYBLUE: Color = Color {
        r: 0x87,
        g: 0xCE,
        b: 0xFA,
        a: 0xFF,
    };
    pub const SKYBLUE: Color = Color {
        r: 0x87,
        g: 0xCE,
        b: 0xEB,
        a: 0xFF,
    };
    pub const DEEPSKYBLUE: Color = Color {
        r: 0x00,
        g: 0xBF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const LIGHTSTEELBLUE: Color = Color {
        r: 0xB0,
        g: 0xC4,
        b: 0xDE,
        a: 0xFF,
    };
    pub const DODGERBLUE: Color = Color {
        r: 0x1E,
        g: 0x90,
        b: 0xFF,
        a: 0xFF,
    };
    pub const CORNFLOWERBLUE: Color = Color {
        r: 0x64,
        g: 0x95,
        b: 0xED,
        a: 0xFF,
    };
    pub const STEELBLUE: Color = Color {
        r: 0x46,
        g: 0x82,
        b: 0xB4,
        a: 0xFF,
    };
    pub const ROYALBLUE: Color = Color {
        r: 0x41,
        g: 0x69,
        b: 0xE1,
        a: 0xFF,
    };
    pub const BLUE: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0xFF,
        a: 0xFF,
    };
    pub const MEDIUMBLUE: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0xCD,
        a: 0xFF,
    };
    pub const DARKBLUE: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0x8B,
        a: 0xFF,
    };
    pub const NAVY: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0x80,
        a: 0xFF,
    };
    pub const MIDNIGHTBLUE: Color = Color {
        r: 0x19,
        g: 0x19,
        b: 0x70,
        a: 0xFF,
    };
    pub const MEDIUMSLATEBLUE: Color = Color {
        r: 0x7B,
        g: 0x68,
        b: 0xEE,
        a: 0xFF,
    };
    pub const SLATEBLUE: Color = Color {
        r: 0x6A,
        g: 0x5A,
        b: 0xCD,
        a: 0xFF,
    };
    pub const DARKSLATEBLUE: Color = Color {
        r: 0x48,
        g: 0x3D,
        b: 0x8B,
        a: 0xFF,
    };

    /// Purple CSS colors //////////////////////////////////////////////////////
    pub const LAVENDER: Color = Color {
        r: 0xE6,
        g: 0xE6,
        b: 0xFA,
        a: 0xFF,
    };
    pub const THISTLE: Color = Color {
        r: 0xD8,
        g: 0xBF,
        b: 0xD8,
        a: 0xFF,
    };
    pub const PLUM: Color = Color {
        r: 0xDD,
        g: 0xA0,
        b: 0xDD,
        a: 0xFF,
    };
    pub const VIOLET: Color = Color {
        r: 0xEE,
        g: 0x82,
        b: 0xEE,
        a: 0xFF,
    };
    pub const ORCHID: Color = Color {
        r: 0xDA,
        g: 0x70,
        b: 0xD6,
        a: 0xFF,
    };
    pub const FUCHSIA: Color = Color {
        r: 0xFF,
        g: 0x00,
        b: 0xFF,
        a: 0xFF,
    };
    pub const MAGENTA: Color = Color {
        r: 0xFF,
        g: 0x00,
        b: 0xFF,
        a: 0xFF,
    };
    pub const MEDIUMORCHID: Color = Color {
        r: 0xBA,
        g: 0x55,
        b: 0xD3,
        a: 0xFF,
    };
    pub const MEDIUMPURPLE: Color = Color {
        r: 0x93,
        g: 0x70,
        b: 0xDB,
        a: 0xFF,
    };
    pub const BLUEVIOLET: Color = Color {
        r: 0x8A,
        g: 0x2B,
        b: 0xE2,
        a: 0xFF,
    };
    pub const DARKVIOLET: Color = Color {
        r: 0x94,
        g: 0x00,
        b: 0xD3,
        a: 0xFF,
    };
    pub const DARKORCHID: Color = Color {
        r: 0x99,
        g: 0x32,
        b: 0xCC,
        a: 0xFF,
    };
    pub const DARKMAGENTA: Color = Color {
        r: 0x8B,
        g: 0x00,
        b: 0x8B,
        a: 0xFF,
    };
    pub const PURPLE: Color = Color {
        r: 0x80,
        g: 0x00,
        b: 0x80,
        a: 0xFF,
    };
    pub const INDIGO: Color = Color {
        r: 0x4B,
        g: 0x00,
        b: 0x82,
        a: 0xFF,
    };

    /// Pink CSS colors ////////////////////////////////////////////////////////
    pub const PINK: Color = Color {
        r: 0xFF,
        g: 0xC0,
        b: 0xCB,
        a: 0xFF,
    };
    pub const LIGHTPINK: Color = Color {
        r: 0xFF,
        g: 0xB6,
        b: 0xC1,
        a: 0xFF,
    };
    pub const HOTPINK: Color = Color {
        r: 0xFF,
        g: 0x69,
        b: 0xB4,
        a: 0xFF,
    };
    pub const DEEPPINK: Color = Color {
        r: 0xFF,
        g: 0x14,
        b: 0x93,
        a: 0xFF,
    };
    pub const PALEVIOLETRED: Color = Color {
        r: 0xDB,
        g: 0x70,
        b: 0x93,
        a: 0xFF,
    };
    pub const MEDIUMVIOLETRED: Color = Color {
        r: 0xC7,
        g: 0x15,
        b: 0x85,
        a: 0xFF,
    };

    /// White CSS colors ///////////////////////////////////////////////////////
    pub const WHITE: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const SNOW: Color = Color {
        r: 0xFF,
        g: 0xFA,
        b: 0xFA,
        a: 0xFF,
    };
    pub const HONEYDEW: Color = Color {
        r: 0xF0,
        g: 0xFF,
        b: 0xF0,
        a: 0xFF,
    };
    pub const MINTCREAM: Color = Color {
        r: 0xF5,
        g: 0xFF,
        b: 0xFA,
        a: 0xFF,
    };
    pub const AZURE: Color = Color {
        r: 0xF0,
        g: 0xFF,
        b: 0xFF,
        a: 0xFF,
    };
    pub const ALICEBLUE: Color = Color {
        r: 0xF0,
        g: 0xF8,
        b: 0xFF,
        a: 0xFF,
    };
    pub const GHOSTWHITE: Color = Color {
        r: 0xF8,
        g: 0xF8,
        b: 0xFF,
        a: 0xFF,
    };
    pub const WHITESMOKE: Color = Color {
        r: 0xF5,
        g: 0xF5,
        b: 0xF5,
        a: 0xFF,
    };
    pub const SEASHELL: Color = Color {
        r: 0xFF,
        g: 0xF5,
        b: 0xEE,
        a: 0xFF,
    };
    pub const BEIGE: Color = Color {
        r: 0xF5,
        g: 0xF5,
        b: 0xDC,
        a: 0xFF,
    };
    pub const OLDLACE: Color = Color {
        r: 0xFD,
        g: 0xF5,
        b: 0xE6,
        a: 0xFF,
    };
    pub const FLORALWHITE: Color = Color {
        r: 0xFF,
        g: 0xFA,
        b: 0xF0,
        a: 0xFF,
    };
    pub const IVORY: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0xF0,
        a: 0xFF,
    };
    pub const ANTIQUEWHITE: Color = Color {
        r: 0xFA,
        g: 0xEB,
        b: 0xD7,
        a: 0xFF,
    };
    pub const LINEN: Color = Color {
        r: 0xFA,
        g: 0xF0,
        b: 0xE6,
        a: 0xFF,
    };
    pub const LAVENDERBLUSH: Color = Color {
        r: 0xFF,
        g: 0xF0,
        b: 0xF5,
        a: 0xFF,
    };
    pub const MISTYROSE: Color = Color {
        r: 0xFF,
        g: 0xE4,
        b: 0xE1,
        a: 0xFF,
    };

    /// Gray CSS colors ////////////////////////////////////////////////////////
    pub const GAINSBORO: Color = Color {
        r: 0xDC,
        g: 0xDC,
        b: 0xDC,
        a: 0xFF,
    };
    pub const LIGHTGRAY: Color = Color {
        r: 0xD3,
        g: 0xD3,
        b: 0xD3,
        a: 0xFF,
    };
    pub const SILVER: Color = Color {
        r: 0xC0,
        g: 0xC0,
        b: 0xC0,
        a: 0xFF,
    };
    pub const DARKGRAY: Color = Color {
        r: 0xA9,
        g: 0xA9,
        b: 0xA9,
        a: 0xFF,
    };
    pub const GRAY: Color = Color {
        r: 0x80,
        g: 0x80,
        b: 0x80,
        a: 0xFF,
    };
    pub const DIMGRAY: Color = Color {
        r: 0x69,
        g: 0x69,
        b: 0x69,
        a: 0xFF,
    };
    pub const LIGHTSLATEGRAY: Color = Color {
        r: 0x77,
        g: 0x88,
        b: 0x99,
        a: 0xFF,
    };
    pub const SLATEGRAY: Color = Color {
        r: 0x70,
        g: 0x80,
        b: 0x90,
        a: 0xFF,
    };
    pub const DARKSLATEGRAY: Color = Color {
        r: 0x2F,
        g: 0x4F,
        b: 0x4F,
        a: 0xFF,
    };
    pub const BLACK: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };

    /// Brown CSS colors ///////////////////////////////////////////////////////
    pub const CORNSILK: Color = Color {
        r: 0xFF,
        g: 0xF8,
        b: 0xDC,
        a: 0xFF,
    };
    pub const BLANCHEDALMOND: Color = Color {
        r: 0xFF,
        g: 0xEB,
        b: 0xCD,
        a: 0xFF,
    };
    pub const BISQUE: Color = Color {
        r: 0xFF,
        g: 0xE4,
        b: 0xC4,
        a: 0xFF,
    };
    pub const NAVAJOWHITE: Color = Color {
        r: 0xFF,
        g: 0xDE,
        b: 0xAD,
        a: 0xFF,
    };
    pub const WHEAT: Color = Color {
        r: 0xF5,
        g: 0xDE,
        b: 0xB3,
        a: 0xFF,
    };
    pub const BURLYWOOD: Color = Color {
        r: 0xDE,
        g: 0xB8,
        b: 0x87,
        a: 0xFF,
    };
    pub const TAN: Color = Color {
        r: 0xD2,
        g: 0xB4,
        b: 0x8C,
        a: 0xFF,
    };
    pub const ROSYBROWN: Color = Color {
        r: 0xBC,
        g: 0x8F,
        b: 0x8F,
        a: 0xFF,
    };
    pub const SANDYBROWN: Color = Color {
        r: 0xF4,
        g: 0xA4,
        b: 0x60,
        a: 0xFF,
    };
    pub const GOLDENROD: Color = Color {
        r: 0xDA,
        g: 0xA5,
        b: 0x20,
        a: 0xFF,
    };
    pub const PERU: Color = Color {
        r: 0xCD,
        g: 0x85,
        b: 0x3F,
        a: 0xFF,
    };
    pub const CHOCOLATE: Color = Color {
        r: 0xD2,
        g: 0x69,
        b: 0x1E,
        a: 0xFF,
    };
    pub const SADDLEBROWN: Color = Color {
        r: 0x8B,
        g: 0x45,
        b: 0x13,
        a: 0xFF,
    };
    pub const SIENNA: Color = Color {
        r: 0xA0,
        g: 0x52,
        b: 0x2D,
        a: 0xFF,
    };
    pub const BROWN: Color = Color {
        r: 0xA5,
        g: 0x2A,
        b: 0x2A,
        a: 0xFF,
    };
    pub const MAROON: Color = Color {
        r: 0x80,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };
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

impl Default for Style {
    fn default() -> Style {
        Style {
            foreground: Color::BLACK,
            background: Color::WHITE,
            font_style: FontStyle::empty(),
        }
    }
}

impl StyleModifier {
    /// Applies the other modifier to this one, creating a new modifier.
    /// Values in `other` are preferred.
    pub fn apply(&self, other: StyleModifier) -> StyleModifier {
        StyleModifier {
            foreground: other.foreground.or(self.foreground),
            background: other.background.or(self.background),
            font_style: other.font_style.or(self.font_style),
        }
    }
}
