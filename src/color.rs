//! 色の表現と書式変換

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 8bit RGB。AviUtl2 の色項目は不透明色だけを持つ（`.aup2` では `rrggbb` の 6 桁小文字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// 本体の保存形式（`rrggbb` 小文字）。色項目への書き込みもこの形式。
    pub fn hex(self) -> String {
        format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// 色項目の値を読む。透明色（空値）や 6 桁 16 進でないものは `None`。
    pub fn from_item_value(value: &str) -> Option<Self> {
        let v = value.trim();
        if v.len() != 6 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Self::from_hex6(v)
    }

    /// 人が入力・貼り付けした文字列を読む。
    /// `rrggbb` / `#rrggbb` / `0xrrggbb` / `<#rrggbb>` / `r,g,b` を受け付ける（大文字小文字・前後の空白は無視）。
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        if let Some(rgb) = Self::parse_tuple(s) {
            return Some(rgb);
        }
        let s = s.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(s);
        let s = s.strip_prefix('#').unwrap_or(s);
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Self::from_hex6(s)
    }

    fn parse_tuple(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            return None;
        }
        let mut v = [0u8; 3];
        for (slot, part) in v.iter_mut().zip(parts) {
            *slot = part.parse().ok()?;
        }
        Some(Self::new(v[0], v[1], v[2]))
    }

    fn from_hex6(s: &str) -> Option<Self> {
        let n = u32::from_str_radix(s, 16).ok()?;
        Some(Self::new((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }

    /// 色相 [0,360)、彩度・明度 [0,1]。
    pub fn hsv(self) -> (f32, f32, f32) {
        let r = self.r as f32 / 255.0;
        let g = self.g as f32 / 255.0;
        let b = self.b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let h = if d <= f32::EPSILON {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        let s = if max <= f32::EPSILON { 0.0 } else { d / max };
        (h, s, max)
    }

    /// 相対輝度（ラベル文字を白黒どちらで描くかの判定用）。
    pub fn luminance(self) -> f32 {
        0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color: {s}")))
    }
}

/// クリップボードへ書き出す形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CopyFormat {
    /// `rrggbb`（オブジェクト設定のカラーコード欄・`.aup2` と同じ）
    #[default]
    Plain,
    /// `#rrggbb`
    Hash,
    /// `<#rrggbb>`（テキストの制御文字）
    TextTag,
    /// `0xrrggbb`（Lua）
    Lua,
    /// `r,g,b`
    Tuple,
}

impl CopyFormat {
    pub const ALL: [CopyFormat; 5] = [
        CopyFormat::Plain,
        CopyFormat::Hash,
        CopyFormat::TextTag,
        CopyFormat::Lua,
        CopyFormat::Tuple,
    ];

    pub fn format(self, rgb: Rgb) -> String {
        match self {
            CopyFormat::Plain => rgb.hex(),
            CopyFormat::Hash => format!("#{}", rgb.hex()),
            CopyFormat::TextTag => format!("<#{}>", rgb.hex()),
            CopyFormat::Lua => format!("0x{}", rgb.hex()),
            CopyFormat::Tuple => format!("{},{},{}", rgb.r, rgb.g, rgb.b),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::Plain => "rrggbb（カラーコード欄）",
            CopyFormat::Hash => "#rrggbb",
            CopyFormat::TextTag => "<#rrggbb>（テキスト制御文字）",
            CopyFormat::Lua => "0xrrggbb（Lua）",
            CopyFormat::Tuple => "r,g,b",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_value_accepts_only_six_hex_digits() {
        assert_eq!(Rgb::from_item_value("1a2a5e"), Some(Rgb::new(0x1a, 0x2a, 0x5e)));
        assert_eq!(Rgb::from_item_value("FFFFFF"), Some(Rgb::new(255, 255, 255)));
        // 透明色（nil）は空値で保存される
        assert_eq!(Rgb::from_item_value(""), None);
        assert_eq!(Rgb::from_item_value("#ffffff"), None);
        assert_eq!(Rgb::from_item_value("12345"), None);
        assert_eq!(Rgb::from_item_value("gggggg"), None);
    }

    #[test]
    fn parse_accepts_common_notations() {
        let c = Some(Rgb::new(0xff, 0x80, 0x00));
        for s in ["ff8000", "#FF8000", "0xff8000", "<#ff8000>", " ff8000 ", "255,128,0", "255, 128, 0"] {
            assert_eq!(Rgb::parse(s), c, "{s}");
        }
        assert_eq!(Rgb::parse("256,0,0"), None);
        assert_eq!(Rgb::parse("#fff"), None);
    }

    #[test]
    fn copy_formats_round_trip_through_parse() {
        let c = Rgb::new(0x12, 0xab, 0xef);
        for f in CopyFormat::ALL {
            assert_eq!(Rgb::parse(&f.format(c)), Some(c), "{f:?}");
        }
        assert_eq!(CopyFormat::Plain.format(c), "12abef");
    }

    #[test]
    fn hsv_of_primaries() {
        let (h, s, v) = Rgb::new(255, 0, 0).hsv();
        assert!((h - 0.0).abs() < 1e-3 && (s - 1.0).abs() < 1e-3 && (v - 1.0).abs() < 1e-3);
        let (h, _, _) = Rgb::new(0, 255, 0).hsv();
        assert!((h - 120.0).abs() < 1e-3);
        let (h, _, _) = Rgb::new(0, 0, 255).hsv();
        assert!((h - 240.0).abs() < 1e-3);
        let (_, s, _) = Rgb::new(128, 128, 128).hsv();
        assert!(s.abs() < 1e-3);
    }

    #[test]
    fn serde_uses_hex_string() {
        let json = serde_json::to_string(&Rgb::new(1, 2, 3)).unwrap();
        assert_eq!(json, "\"010203\"");
        let back: Rgb = serde_json::from_str("\"#010203\"").unwrap();
        assert_eq!(back, Rgb::new(1, 2, 3));
    }
}
