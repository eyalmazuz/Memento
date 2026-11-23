//! Structured font CSV and ARGB color codecs shared between `subomi-config`
//! callers and the Settings UI.

use std::fmt;

/// Structured font specification serialized as a 10-field CSV string
/// (`family,pointSizeF,-1,styleHint,weight,style,underline,strikeOut,fixedPitch,rawMode`),
/// also reading legacy 17-field OpenType-weight CSV strings.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    pub point_size: f64,
    pub bold: bool,
    pub italic: bool,
}

impl FontSpec {
    pub fn new(family: impl Into<String>, point_size: f64, bold: bool, italic: bool) -> Self {
        Self {
            family: family.into(),
            point_size: if point_size.is_finite() && point_size > 0.0 {
                point_size
            } else {
                12.0
            },
            bold,
            italic,
        }
    }

    /// Parses a comma-separated font spec (10-field, 17-field, or bare family name).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("font specification cannot be empty".to_owned());
        }
        let parts: Vec<&str> = trimmed.split(',').collect();
        let family = parts[0].trim();
        if family.is_empty() {
            return Err("font family cannot be empty".to_owned());
        }

        let point_size = parts
            .get(1)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| {
                parts
                    .get(2)
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v > 0.0)
            })
            .unwrap_or(12.0);

        let weight = parts
            .get(4)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(50);
        // Qt5 weights are 0..=99 (Normal=50, Bold=75); Qt6 weights are 100..=1000 (Normal=400, Bold=700).
        let bold = if weight >= 100 {
            weight >= 600
        } else {
            weight >= 63
        };

        let style = parts
            .get(5)
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let italic = style != 0;

        Ok(Self {
            family: family.to_owned(),
            point_size,
            bold,
            italic,
        })
    }

    /// Formats the point size cleanly (integer when exact, trimmed decimal otherwise).
    #[must_use]
    pub fn formatted_size(&self) -> String {
        let size = if self.point_size.is_finite() && self.point_size > 0.0 {
            self.point_size
        } else {
            12.0
        };
        if (size - size.round()).abs() < 1e-6 {
            format!("{}", size.round() as i64)
        } else {
            let formatted = format!("{size:.2}");
            formatted
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        }
    }

    /// Serializes into the 10-field CSV font specification format.
    #[must_use]
    pub fn to_csv_string(&self) -> String {
        self.to_csv_string_preserving(None)
    }

    /// Serializes into the 10-field CSV font specification format while
    /// preserving `styleHint` and trailing flags (`underline`, `strikeOut`,
    /// `fixedPitch`, `rawMode`) from `existing_raw` when present.
    #[must_use]
    pub fn to_csv_string_preserving(&self, existing_raw: Option<&str>) -> String {
        let parts: Vec<&str> = existing_raw
            .map(|raw| raw.trim().split(',').collect())
            .unwrap_or_default();
        let field_or = |idx: usize, fallback: &str| -> String {
            parts
                .get(idx)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback)
                .to_owned()
        };

        let family = self.family.trim();
        let family = if family.is_empty() { "Sans Serif" } else { family };
        let size = self.formatted_size();
        let style_hint = field_or(3, "5");
        let weight = if self.bold { 75 } else { 50 };
        let style = if self.italic { 1 } else { 0 };
        let underline = field_or(6, "0");
        let strike_out = field_or(7, "0");
        let fixed_pitch = field_or(8, "0");
        let raw_mode = field_or(9, "0");

        format!(
            "{family},{size},-1,{style_hint},{weight},{style},{underline},{strike_out},{fixed_pitch},{raw_mode}"
        )
    }
}

/// ARGB color value matching Qt `QColor` / Subomi `#AARRGGBB` serialization.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ArgbColor {
    pub a: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ArgbColor {
    pub const fn new(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self { a, r, g, b }
    }

    /// Parses `#AARRGGBB`, `#RRGGBB`, `#ARGB`, `#RGB`, or basic SVG color names.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("color value cannot be empty".to_owned());
        }
        if let Some(hex) = trimmed.strip_prefix('#') {
            let bytes = hex.as_bytes();
            return match bytes.len() {
                3 => {
                    let r = parse_hex_nibble(bytes[0])? * 17;
                    let g = parse_hex_nibble(bytes[1])? * 17;
                    let b = parse_hex_nibble(bytes[2])? * 17;
                    Ok(Self::new(255, r, g, b))
                }
                4 => {
                    let a = parse_hex_nibble(bytes[0])? * 17;
                    let r = parse_hex_nibble(bytes[1])? * 17;
                    let g = parse_hex_nibble(bytes[2])? * 17;
                    let b = parse_hex_nibble(bytes[3])? * 17;
                    Ok(Self::new(a, r, g, b))
                }
                6 => {
                    let r = parse_hex_byte(bytes[0], bytes[1])?;
                    let g = parse_hex_byte(bytes[2], bytes[3])?;
                    let b = parse_hex_byte(bytes[4], bytes[5])?;
                    Ok(Self::new(255, r, g, b))
                }
                8 => {
                    let a = parse_hex_byte(bytes[0], bytes[1])?;
                    let r = parse_hex_byte(bytes[2], bytes[3])?;
                    let g = parse_hex_byte(bytes[4], bytes[5])?;
                    let b = parse_hex_byte(bytes[6], bytes[7])?;
                    Ok(Self::new(a, r, g, b))
                }
                _ => Err(format!(
                    "invalid hex color {raw:?} (expected #RGB, #ARGB, #RRGGBB, or #AARRGGBB)"
                )),
            };
        }

        match trimmed.to_ascii_lowercase().as_str() {
            "transparent" => Ok(Self::new(0, 0, 0, 0)),
            "black" => Ok(Self::new(255, 0, 0, 0)),
            "white" => Ok(Self::new(255, 255, 255, 255)),
            "red" => Ok(Self::new(255, 255, 0, 0)),
            "green" => Ok(Self::new(255, 0, 128, 0)),
            "blue" => Ok(Self::new(255, 0, 0, 255)),
            "yellow" => Ok(Self::new(255, 255, 255, 0)),
            "cyan" => Ok(Self::new(255, 0, 255, 255)),
            "magenta" => Ok(Self::new(255, 255, 0, 255)),
            "gray" | "grey" => Ok(Self::new(255, 128, 128, 128)),
            _ => Err(format!("invalid color value: {raw:?}")),
        }
    }

    /// Formats as `#AARRGGBB` uppercase hex (Qt / Subomi `subomi.conf` format).
    #[must_use]
    pub fn to_argb_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}{:02X}", self.a, self.r, self.g, self.b)
    }

    /// Formats as `#RRGGBB` uppercase hex.
    #[must_use]
    pub fn to_rgb_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

impl fmt::Display for ArgbColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02X}{:02X}{:02X}{:02X}",
            self.a, self.r, self.g, self.b
        )
    }
}

fn parse_hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex digit byte 0x{byte:02x}")),
    }
}

fn parse_hex_byte(hi: u8, lo: u8) -> Result<u8, String> {
    Ok((parse_hex_nibble(hi)? << 4) | parse_hex_nibble(lo)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_spec_round_trips_10_field_and_parses_17_field_weights() {
        let default_sub = "Noto Sans CJK JP,24,-1,5,50,0,0,0,0,0";
        let parsed = FontSpec::parse(default_sub).unwrap();
        assert_eq!(parsed.family, "Noto Sans CJK JP");
        assert_eq!(parsed.point_size, 24.0);
        assert!(!parsed.bold);
        assert!(!parsed.italic);
        assert_eq!(parsed.to_csv_string(), default_sub);

        let bold_tag = "Noto Sans CJK JP,10,-1,5,75,0,0,0,0,0";
        let parsed_bold = FontSpec::parse(bold_tag).unwrap();
        assert!(parsed_bold.bold);
        assert!(!parsed_bold.italic);
        assert_eq!(parsed_bold.to_csv_string(), bold_tag);

        let ext_normal = "Noto Sans,14.5,-1,5,400,0,0,0,0,0,0,0,0,0,0,1,Regular";
        let parsed_ext_normal = FontSpec::parse(ext_normal).unwrap();
        assert_eq!(parsed_ext_normal.point_size, 14.5);
        assert!(!parsed_ext_normal.bold);
        assert!(!parsed_ext_normal.italic);
        assert_eq!(
            parsed_ext_normal.to_csv_string(),
            "Noto Sans,14.5,-1,5,50,0,0,0,0,0"
        );

        let ext_bold_italic = "Noto Sans,16,-1,5,700,1,1,0,0,0,0,0,0,0,0,1,Bold Italic";
        let parsed_ext_bi = FontSpec::parse(ext_bold_italic).unwrap();
        assert!(parsed_ext_bi.bold);
        assert!(parsed_ext_bi.italic);
        assert_eq!(
            parsed_ext_bi.to_csv_string_preserving(Some(ext_bold_italic)),
            "Noto Sans,16,-1,5,75,1,1,0,0,0"
        );
    }

    #[test]
    fn argb_color_parses_and_normalizes_qt_hex() {
        assert_eq!(
            ArgbColor::parse("#FFFFFFFF").unwrap(),
            ArgbColor::new(255, 255, 255, 255)
        );
        assert_eq!(
            ArgbColor::parse("#00000000").unwrap(),
            ArgbColor::new(0, 0, 0, 0)
        );
        assert_eq!(
            ArgbColor::parse("#1A3B5C").unwrap().to_argb_hex(),
            "#FF1A3B5C"
        );
        assert_eq!(
            ArgbColor::parse("#80ff0080").unwrap().to_argb_hex(),
            "#80FF0080"
        );
        assert_eq!(
            ArgbColor::parse("#abc").unwrap().to_argb_hex(),
            "#FFAABBCC"
        );
        assert_eq!(
            ArgbColor::parse("transparent").unwrap().to_argb_hex(),
            "#00000000"
        );
        assert!(ArgbColor::parse("#12345").is_err());
        assert!(ArgbColor::parse("not-a-color").is_err());
    }
}
