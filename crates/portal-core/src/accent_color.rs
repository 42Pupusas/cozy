//! `org.freedesktop.appearance` `accent-color`: the system's preferred
//! accent, as the spec's `(ddd)` sRGB triple in `[0, 1]`.
//!
//! A distinct type from [`crate::color_scheme::ColorScheme`] for the same
//! reason `bins/files`' `Theme` keeps `accent` and `accent_surface` as
//! separate fields rather than one "accent" grab-bag: this crate's own
//! `AppearanceSettings` needs to hand each key to `ReadAll`/`Read` as an
//! independently-typed value, and folding the double-to-`Value` conversion
//! in here keeps that arithmetic out of the dispatch wiring, where a
//! wrong-signature mistake would only surface as an obscure `InvalidArgs`
//! from a real caller instead of a unit test failure here.

use config::Color;
use dbus::value::Value;

/// The system's preferred accent color, as three `[0.0, 1.0]` sRGB
/// components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccentColor {
    r: f64,
    g: f64,
    b: f64,
}

impl AccentColor {
    /// Convert a 24-bit [`Color`] (this workspace's own config type) to the
    /// portal's `[0, 1]` double triple.
    #[must_use]
    pub fn from_config_color(color: Color) -> Self {
        Self {
            r: f64::from(color.r) / 255.0,
            g: f64::from(color.g) / 255.0,
            b: f64::from(color.b) / 255.0,
        }
    }

    /// The `(ddd)` struct value a `Read`/`ReadAll` reply carries for this
    /// key.
    #[must_use]
    pub fn as_value(self) -> Value {
        Value::Struct(vec![
            Value::Double(self.r),
            Value::Double(self.g),
            Value::Double(self.b),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_white_maps_to_all_ones() {
        let accent = AccentColor::from_config_color(Color::from_rgb(0xff, 0xff, 0xff));
        assert_eq!(
            accent,
            AccentColor {
                r: 1.0,
                g: 1.0,
                b: 1.0
            }
        );
    }

    #[test]
    fn black_maps_to_all_zeros() {
        let accent = AccentColor::from_config_color(Color::from_rgb(0, 0, 0));
        assert_eq!(
            accent,
            AccentColor {
                r: 0.0,
                g: 0.0,
                b: 0.0
            }
        );
    }

    #[test]
    fn as_value_produces_the_ddd_struct_shape() {
        let accent = AccentColor::from_config_color(Color::from_rgb(0x89, 0xb4, 0xfa));
        let value = accent.as_value();
        let Value::Struct(fields) = &value else {
            panic!("expected a Struct value");
        };
        assert_eq!(fields.len(), 3);
        assert_eq!(value.signature(), "(ddd)");
    }

    #[test]
    fn known_accent_round_trips_through_the_expected_fractions() {
        // 0x89 = 137, 0xb4 = 180, 0xfa = 250 -- picked from `bins/files`'
        // own default accent so this test ties to a value that actually
        // ships, not an arbitrary one.
        let accent = AccentColor::from_config_color(Color::from_rgb(0x89, 0xb4, 0xfa));
        let Value::Struct(fields) = accent.as_value() else {
            unreachable!()
        };
        let Value::Double(r) = fields[0] else {
            panic!()
        };
        let Value::Double(g) = fields[1] else {
            panic!()
        };
        let Value::Double(b) = fields[2] else {
            panic!()
        };
        assert!((r - 137.0 / 255.0).abs() < f64::EPSILON);
        assert!((g - 180.0 / 255.0).abs() < f64::EPSILON);
        assert!((b - 250.0 / 255.0).abs() < f64::EPSILON);
    }
}
