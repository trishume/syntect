// Code based on [https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs](https://github.com/defuz/sublimate/blob/master/src/core/syntax/scope.rs)
// released under the MIT license by @defuz
use serde_derive::{Deserialize, Serialize};

/// Foreground and background colors, with font style
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    /// Foreground color
    pub foreground: Color,
    /// Background color
    pub background: Color,
    /// Style of the font
    pub font_style: FontStyle,
}

/// A change to a [`Style`] applied incrementally by a theme rule
///
/// Fields left empty (as `None`) will not modify the corresponding field on a `Style`
///
/// [`Style`]: struct.Style.html
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleModifier {
    /// Foreground color
    pub foreground: Option<Color>,
    /// Background color
    pub background: Option<Color>,
    /// Style of the font
    pub font_style: Option<FontStyle>,
}

/// RGBA color, directly from the theme
///
/// Because these numbers come directly from the theme, you might have to do your own color space
/// conversion if you're outputting a different color space from the theme. This can be a problem
/// because some Sublime themes use sRGB and some don't. This is specified in an attribute syntect
/// doesn't parse yet.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    /// Red component
    pub r: u8,
    /// Green component
    pub g: u8,
    /// Blue component
    pub b: u8,
    /// Alpha (transparency) component
    pub a: u8,
}

// More compact alternate debug representation by not using a separate line for each color field,
// also adapts the default debug representation to match.
impl std::fmt::Debug for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let Color { r, g, b, a } = self;
        if f.alternate() {
            // when formatted with "{:#?}"
            write!(
                f,
                "Color {{ r/g/b/a: {: >3}/{: >3}/{: >3}/{: >3} }}",
                r, g, b, a
            )
        } else {
            // when formatted with "{:?}"
            write!(f, "Color {{ r/g/b/a: {}/{}/{}/{} }}", r, g, b, a)
        }
    }
}

/// The color-independent styling of a font - i.e. bold, italicized, and/or underlined
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FontStyle {
    bits: u8,
}

impl core::fmt::Debug for FontStyle {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter,
    ) -> core::fmt::Result {
        #[allow(non_snake_case)]
        trait __BitFlags {
            #[inline]
            fn BOLD(&self) -> bool {
                false
            }
            #[inline]
            fn UNDERLINE(&self) -> bool {
                false
            }
            #[inline]
            fn ITALIC(&self) -> bool {
                false
            }
        }
        #[allow(non_snake_case)]
        impl __BitFlags for FontStyle {
            #[allow(deprecated)]
            #[inline]
            fn BOLD(&self) -> bool {
                if Self::BOLD.bits == 0 && self.bits != 0 {
                    false
                } else {
                    self.bits & Self::BOLD.bits == Self::BOLD.bits
                }
            }
            #[allow(deprecated)]
            #[inline]
            fn UNDERLINE(&self) -> bool {
                if Self::UNDERLINE.bits == 0 && self.bits != 0 {
                    false
                } else {
                    self.bits & Self::UNDERLINE.bits == Self::UNDERLINE.bits
                }
            }
            #[allow(deprecated)]
            #[inline]
            fn ITALIC(&self) -> bool {
                if Self::ITALIC.bits == 0 && self.bits != 0 {
                    false
                } else {
                    self.bits & Self::ITALIC.bits == Self::ITALIC.bits
                }
            }
        }
        let mut first = true;
        if <Self as __BitFlags>::BOLD(self) {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str("BOLD")?;
        }
        if <Self as __BitFlags>::UNDERLINE(self) {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str("UNDERLINE")?;
        }
        if <Self as __BitFlags>::ITALIC(self) {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str("ITALIC")?;
        }
        let extra_bits = self.bits & !Self::all().bits();
        if extra_bits != 0 {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            f.write_str("0x")?;
            core::fmt::LowerHex::fmt(&extra_bits, f)?;
        }
        if first {
            f.write_str("(empty)")?;
        }
        Ok(())
    }
}
impl core::fmt::Binary for FontStyle {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter,
    ) -> core::fmt::Result {
        core::fmt::Binary::fmt(&self.bits, f)
    }
}
impl core::fmt::Octal for FontStyle {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter,
    ) -> core::fmt::Result {
        core::fmt::Octal::fmt(&self.bits, f)
    }
}
impl core::fmt::LowerHex for FontStyle {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter,
    ) -> core::fmt::Result {
        core::fmt::LowerHex::fmt(&self.bits, f)
    }
}
impl core::fmt::UpperHex for FontStyle {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter,
    ) -> core::fmt::Result {
        core::fmt::UpperHex::fmt(&self.bits, f)
    }
}
#[allow(dead_code)]
impl FontStyle {
    /// Bold font style
    pub const BOLD: Self = Self { bits: 1 };
    /// Underline font style
    pub const UNDERLINE: Self = Self { bits: 2 };
    /// Italic font style
    pub const ITALIC: Self = Self { bits: 4 };
    /// Returns an empty set of flags.
    #[inline]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }
    /// Returns the set containing all flags.
    #[inline]
    pub const fn all() -> Self {
        #[allow(non_snake_case)]
        trait __BitFlags {
            const BOLD: u8 = 0;
            const UNDERLINE: u8 = 0;
            const ITALIC: u8 = 0;
        }
        #[allow(non_snake_case)]
        impl __BitFlags for FontStyle {
            #[allow(deprecated)]
            const BOLD: u8 = Self::BOLD.bits;
            #[allow(deprecated)]
            const UNDERLINE: u8 = Self::UNDERLINE.bits;
            #[allow(deprecated)]
            const ITALIC: u8 = Self::ITALIC.bits;
        }
        Self {
            bits: <Self as __BitFlags>::BOLD | <Self as __BitFlags>::UNDERLINE
                | <Self as __BitFlags>::ITALIC,
        }
    }
    /// Returns the raw value of the flags currently stored.
    #[inline]
    pub const fn bits(&self) -> u8 {
        self.bits
    }
    /// Convert from underlying bit representation, unless that
    /// representation contains bits that do not correspond to a flag.
    #[inline]
    pub const fn from_bits(bits: u8) -> core::option::Option<Self> {
        if (bits & !Self::all().bits()) == 0 {
            core::option::Option::Some(Self { bits })
        } else {
            core::option::Option::None
        }
    }
    /// Convert from underlying bit representation, dropping any bits
    /// that do not correspond to flags.
    #[inline]
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self {
            bits: bits & Self::all().bits,
        }
    }
    /// Convert from underlying bit representation, preserving all
    /// bits (even those not corresponding to a defined flag).
    ///
    /// # Safety
    ///
    /// The caller of the `bitflags!` macro can chose to allow or
    /// disallow extra bits for their bitflags type.
    ///
    /// The caller of `from_bits_unchecked()` has to ensure that
    /// all bits correspond to a defined flag or that extra bits
    /// are valid for this bitflags type.
    #[inline]
    pub const unsafe fn from_bits_unchecked(bits: u8) -> Self {
        Self { bits }
    }
    /// Returns `true` if no flags are currently stored.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bits() == Self::empty().bits()
    }
    /// Returns `true` if all flags are currently set.
    #[inline]
    pub const fn is_all(&self) -> bool {
        Self::all().bits | self.bits == self.bits
    }
    /// Returns `true` if there are flags common to both `self` and `other`.
    #[inline]
    pub const fn intersects(&self, other: Self) -> bool {
        !(Self {
            bits: self.bits & other.bits,
        })
            .is_empty()
    }
    /// Returns `true` if all of the flags in `other` are contained within `self`.
    #[inline]
    pub const fn contains(&self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }
    /// Inserts the specified flags in-place.
    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.bits |= other.bits;
    }
    /// Removes the specified flags in-place.
    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.bits &= !other.bits;
    }
    /// Toggles the specified flags in-place.
    #[inline]
    pub fn toggle(&mut self, other: Self) {
        self.bits ^= other.bits;
    }
    /// Inserts or removes the specified flags depending on the passed value.
    #[inline]
    pub fn set(&mut self, other: Self, value: bool) {
        if value {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }
    /// Returns the intersection between the flags in `self` and
    /// `other`.
    ///
    /// Specifically, the returned set contains only the flags which are
    /// present in *both* `self` *and* `other`.
    ///
    /// This is equivalent to using the `&` operator (e.g.
    /// [`ops::BitAnd`]), as in `flags & other`.
    ///
    /// [`ops::BitAnd`]: https://doc.rust-lang.org/std/ops/trait.BitAnd.html
    #[inline]
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }
    /// Returns the union of between the flags in `self` and `other`.
    ///
    /// Specifically, the returned set contains all flags which are
    /// present in *either* `self` *or* `other`, including any which are
    /// present in both (see [`Self::symmetric_difference`] if that
    /// is undesirable).
    ///
    /// This is equivalent to using the `|` operator (e.g.
    /// [`ops::BitOr`]), as in `flags | other`.
    ///
    /// [`ops::BitOr`]: https://doc.rust-lang.org/std/ops/trait.BitOr.html
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }
    /// Returns the difference between the flags in `self` and `other`.
    ///
    /// Specifically, the returned set contains all flags present in
    /// `self`, except for the ones present in `other`.
    ///
    /// It is also conceptually equivalent to the "bit-clear" operation:
    /// `flags & !other` (and this syntax is also supported).
    ///
    /// This is equivalent to using the `-` operator (e.g.
    /// [`ops::Sub`]), as in `flags - other`.
    ///
    /// [`ops::Sub`]: https://doc.rust-lang.org/std/ops/trait.Sub.html
    #[inline]
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }
    /// Returns the [symmetric difference][sym-diff] between the flags
    /// in `self` and `other`.
    ///
    /// Specifically, the returned set contains the flags present which
    /// are present in `self` or `other`, but that are not present in
    /// both. Equivalently, it contains the flags present in *exactly
    /// one* of the sets `self` and `other`.
    ///
    /// This is equivalent to using the `^` operator (e.g.
    /// [`ops::BitXor`]), as in `flags ^ other`.
    ///
    /// [sym-diff]: https://en.wikipedia.org/wiki/Symmetric_difference
    /// [`ops::BitXor`]: https://doc.rust-lang.org/std/ops/trait.BitXor.html
    #[inline]
    #[must_use]
    pub const fn symmetric_difference(self, other: Self) -> Self {
        Self {
            bits: self.bits ^ other.bits,
        }
    }
    /// Returns the complement of this set of flags.
    ///
    /// Specifically, the returned set contains all the flags which are
    /// not set in `self`, but which are allowed for this type.
    ///
    /// Alternatively, it can be thought of as the set difference
    /// between [`Self::all()`] and `self` (e.g. `Self::all() - self`)
    ///
    /// This is equivalent to using the `!` operator (e.g.
    /// [`ops::Not`]), as in `!flags`.
    ///
    /// [`Self::all()`]: Self::all
    /// [`ops::Not`]: https://doc.rust-lang.org/std/ops/trait.Not.html
    #[inline]
    #[must_use]
    pub const fn complement(self) -> Self {
        Self::from_bits_truncate(!self.bits)
    }
}
impl core::ops::BitOr for FontStyle {
    type Output = Self;
    /// Returns the union of the two sets of flags.
    #[inline]
    fn bitor(self, other: FontStyle) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }
}
impl core::ops::BitOrAssign for FontStyle {
    /// Adds the set of flags.
    #[inline]
    fn bitor_assign(&mut self, other: Self) {
        self.bits |= other.bits;
    }
}
impl core::ops::BitXor for FontStyle {
    type Output = Self;
    /// Returns the left flags, but with all the right flags toggled.
    #[inline]
    fn bitxor(self, other: Self) -> Self {
        Self {
            bits: self.bits ^ other.bits,
        }
    }
}
impl core::ops::BitXorAssign for FontStyle {
    /// Toggles the set of flags.
    #[inline]
    fn bitxor_assign(&mut self, other: Self) {
        self.bits ^= other.bits;
    }
}
impl core::ops::BitAnd for FontStyle {
    type Output = Self;
    /// Returns the intersection between the two sets of flags.
    #[inline]
    fn bitand(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }
}
impl core::ops::BitAndAssign for FontStyle {
    /// Disables all flags disabled in the set.
    #[inline]
    fn bitand_assign(&mut self, other: Self) {
        self.bits &= other.bits;
    }
}
impl core::ops::Sub for FontStyle {
    type Output = Self;
    /// Returns the set difference of the two sets of flags.
    #[inline]
    fn sub(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }
}
impl core::ops::SubAssign for FontStyle {
    /// Disables all flags enabled in the set.
    #[inline]
    fn sub_assign(&mut self, other: Self) {
        self.bits &= !other.bits;
    }
}
impl core::ops::Not for FontStyle {
    type Output = Self;
    /// Returns the complement of this set of flags.
    #[inline]
    fn not(self) -> Self {
        Self { bits: !self.bits } & Self::all()
    }
}
impl core::iter::Extend<FontStyle> for FontStyle {
    fn extend<T: core::iter::IntoIterator<Item = Self>>(
        &mut self,
        iterator: T,
    ) {
        for item in iterator {
            self.insert(item)
        }
    }
}
impl core::iter::FromIterator<FontStyle> for FontStyle {
    fn from_iter<T: core::iter::IntoIterator<Item = Self>>(
        iterator: T,
    ) -> Self {
        let mut result = Self::empty();
        result.extend(iterator);
        result
    }
}

impl Color {
    /// The color black (`#000000`)
    pub const BLACK: Color = Color {
        r: 0x00,
        g: 0x00,
        b: 0x00,
        a: 0xFF,
    };

    /// The color white (`#FFFFFF`)
    pub const WHITE: Color = Color {
        r: 0xFF,
        g: 0xFF,
        b: 0xFF,
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
    ///
    /// Values in `other` are preferred.
    pub fn apply(&self, other: StyleModifier) -> StyleModifier {
        StyleModifier {
            foreground: other.foreground.or(self.foreground),
            background: other.background.or(self.background),
            font_style: other.font_style.or(self.font_style),
        }
    }
}

impl Default for FontStyle {
    fn default() -> FontStyle {
        FontStyle::empty()
    }
}
