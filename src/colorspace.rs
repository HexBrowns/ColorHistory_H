//! 色空間と色差（v0.4.0〜）
//!
//! 式は共有部品の写し。正本は `AI/tools/assets/colorspace.hlsl` と `Script/EffectUtils_ColorSpace.lua`
//! （Lua 側は CSS Color 4 の値と Sharma の CIEDE2000 の試験データで検証済み: `AI/scripts/色空間拡張/verify_colorspace.py`）。
//! ColorSpaces_H にも Oklab・Lab の実装があるが別のクレートなので、ここには要る分だけを短く書く。
//!
//! - Oklab: Björn Ottosson (2020)
//! - Lab: D65 の白（共有部品と同じ定数。CSS の `lab()` は D50 なので値は少し違う）
//! - 色差の番号は共有部品と同じ `0 RGB / 1 YCbCr（BT.709 の色差だけ）/ 2 Oklab / 3 CIEDE2000`。どれも黒と白の差がおよそ 1

use serde::{Deserialize, Serialize};

use crate::color::Rgb;

const TAU: f64 = std::f64::consts::TAU;

fn lin1(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 8bit の各値をリニアにした表（変換のたびに `powf` を呼ばない）
fn linear_of(v: u8) -> f64 {
    static TABLE: std::sync::OnceLock<[f64; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| lin1(i as f64 / 255.0)))[v as usize]
}

fn to_linear(c: Rgb) -> [f64; 3] {
    [linear_of(c.r), linear_of(c.g), linear_of(c.b)]
}

/// sRGB → Oklab `[L, a, b]`
pub fn oklab(c: Rgb) -> [f64; 3] {
    let [r, g, b] = to_linear(c);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// sRGB → Oklch `[L, C, h（度 0〜360）]`
pub fn oklch(c: Rgb) -> [f64; 3] {
    let [l, a, b] = oklab(c);
    [l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0)]
}

/// CIE 15: ε = 216/24389、κ = 24389/27
fn lab_f(t: f64) -> f64 {
    if t > 0.008856452 {
        t.cbrt()
    } else {
        (903.2963 * t + 16.0) / 116.0
    }
}

/// sRGB → CIE Lab（D65）
pub fn lab(c: Rgb) -> [f64; 3] {
    let [r, g, b] = to_linear(c);
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    let (fx, fy, fz) = (lab_f(x / 0.95047), lab_f(y), lab_f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000（Sharma, Wu, Dalal 2005）。Lab の値どうし（ΔE00 の値そのもの。割らない）
pub fn ciede2000(p: [f64; 3], q: [f64; 3]) -> f64 {
    let ([l1, a1, b1], [l2, a2, b2]) = (p, q);
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let cb7 = ((c1 + c2) * 0.5).powi(7);
    let g = 0.5 * (1.0 - (cb7 / (cb7 + 6103515625.0)).sqrt());
    let (a1p, a2p) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let c1p = a1p.hypot(b1);
    let c2p = a2p.hypot(b2);
    let hue = |a: f64, b: f64| {
        if a.abs() + b.abs() > 0.0 {
            b.atan2(a).rem_euclid(TAU)
        } else {
            0.0
        }
    };
    let (h1p, h2p) = (hue(a1p, b1), hue(a2p, b2));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let mut dh = 0.0;
    let mut hbp = h1p + h2p;
    if c1p * c2p > 0.0 {
        dh = h2p - h1p;
        if dh > std::f64::consts::PI {
            dh -= TAU;
        } else if dh < -std::f64::consts::PI {
            dh += TAU;
        }
        hbp = if (h1p - h2p).abs() > std::f64::consts::PI {
            if h1p + h2p < TAU {
                (h1p + h2p + TAU) * 0.5
            } else {
                (h1p + h2p - TAU) * 0.5
            }
        } else {
            (h1p + h2p) * 0.5
        };
    }
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dh * 0.5).sin();
    let lbp = (l1 + l2) * 0.5;
    let cbp = (c1p + c2p) * 0.5;
    let pi = std::f64::consts::PI;
    let t = 1.0 - 0.17 * (hbp - pi / 6.0).cos() + 0.24 * (2.0 * hbp).cos() + 0.32 * (3.0 * hbp + pi / 30.0).cos()
        - 0.20 * (4.0 * hbp - 63.0 * pi / 180.0).cos();
    let dtheta = pi / 6.0 * (-((hbp.to_degrees() - 275.0) / 25.0).powi(2)).exp();
    let cbp7 = cbp.powi(7);
    let rc = 2.0 * (cbp7 / (cbp7 + 6103515625.0)).sqrt();
    let l50 = (lbp - 50.0).powi(2);
    let sl = 1.0 + 0.015 * l50 / (20.0 + l50).sqrt();
    let sc = 1.0 + 0.045 * cbp;
    let sh = 1.0 + 0.015 * cbp * t;
    let rt = -(2.0 * dtheta).sin() * rc;
    let (tl, tc, th) = (dl / sl, dc / sc, dhh / sh);
    (tl * tl + tc * tc + th * th + rt * tc * th).max(0.0).sqrt()
}

/// 一括置換の「色差」。保存キーは `settings.replace_metric`（列挙の名前で保存する）。
///
/// 許容値（UI の「しきい値 ±」、0〜64）は、どの色差でも **8bit の段数**として読む。
/// 0 はどの色差でも完全一致。1 以上は `色差 ≤ N × STEP` で判定する（`STEP` は色差ごとの「8bit の 1 段」の大きさ）。
///
/// `STEP` の決め方: 今まで同じとみなした範囲（各成分 ±N の立方体）の **8 つの角まで入る大きさ**にする。
/// 今まで拾えた色を、色差を変えても落とさないため（丸めで 1 ずつずれた `fe0101` と `ff0000` は N = 1 で拾う）。
/// - RGB: 角までの距離は場所によらず N/255 なので `1/255`。立方体は球の内側にちょうど収まる
/// - YCbCr: 角の Cb・Cr の差の最大（(+1, −1, +1) の角）。線形なので場所によらない。立方体の影（六角形）が円に収まる
/// - Oklab・CIEDE2000: 角までの色差は場所で変わるので、sRGB の格子（各チャンネル 0, 15, 30, …, 255 の 18³ 色）の
///   各色で 8 つの角の最大を取り、その**中央値**を N = 1 で取った値。N を 16 まで広げても中央値 ÷ N は Oklab で 2%・
///   CIEDE2000 で 6% しか変わらないので、N に比例させる
///
/// 値は `tests::metric_steps_cover_the_cube_corners` が同じ格子から計算し直して確かめる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorMetric {
    /// 今まで（〜v0.3.1）: RGB の各成分の差がどれも N 以下
    #[default]
    PerChannel,
    /// 共有部品の 0: RGB のユークリッド距離 ÷ √3
    Rgb,
    /// 共有部品の 1: BT.709 の Cb・Cr の差だけ（明るさを見ない）× √2
    YCbCr,
    /// 共有部品の 2: Oklab のユークリッド距離
    Oklab,
    /// 共有部品の 3: CIEDE2000 ÷ 100（Lab は D65）
    Ciede2000,
}

impl ColorMetric {
    pub const ALL: [ColorMetric; 5] =
        [ColorMetric::PerChannel, ColorMetric::Rgb, ColorMetric::YCbCr, ColorMetric::Oklab, ColorMetric::Ciede2000];

    pub fn label(self) -> &'static str {
        match self {
            ColorMetric::PerChannel => "各成分の差",
            ColorMetric::Rgb => "RGB",
            ColorMetric::YCbCr => "YCbCr",
            ColorMetric::Oklab => "Oklab",
            ColorMetric::Ciede2000 => "CIEDE2000",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ColorMetric::PerChannel => "R・G・B のどの差もしきい値以下なら同じ色とみなす（v0.3 までと同じ）",
            ColorMetric::Rgb => "RGB の距離（各成分の差で拾えた色はすべて拾う）",
            ColorMetric::YCbCr => "Cb・Cr（BT.709）の差だけを見る。明るさだけが違う色はまとめる",
            ColorMetric::Oklab => "Oklab の距離。見た目の差に近い",
            ColorMetric::Ciede2000 => "CIEDE2000（Lab は D65）。見た目の差に近い",
        }
    }

    /// 「8bit の 1 段」に当たる色差（黒と白の差がおよそ 1 の単位）
    pub fn step(self) -> f64 {
        match self {
            ColorMetric::PerChannel => 1.0 / 255.0,
            ColorMetric::Rgb => 1.0 / 255.0,
            ColorMetric::YCbCr => 0.0066072,
            ColorMetric::Oklab => 0.003475,
            ColorMetric::Ciede2000 => 0.004962,
        }
    }
}

/// 色差を計算するための 1 色ぶんの値（色ごとに 1 回だけ変換する）
#[derive(Debug, Clone, Copy, PartialEq)]
enum Coords {
    Srgb(Rgb),
    Oklab([f64; 3]),
    Lab([f64; 3]),
}

fn coords(metric: ColorMetric, c: Rgb) -> Coords {
    match metric {
        ColorMetric::PerChannel | ColorMetric::Rgb | ColorMetric::YCbCr => Coords::Srgb(c),
        ColorMetric::Oklab => Coords::Oklab(oklab(c)),
        ColorMetric::Ciede2000 => Coords::Lab(lab(c)),
    }
}

/// 2 色の色差（共有部品の `distance` と同じ。`PerChannel` は各成分の差の最大 ÷ 255）
pub fn distance(metric: ColorMetric, a: Rgb, b: Rgb) -> f64 {
    distance_of(metric, coords(metric, a), coords(metric, b))
}

fn distance_of(metric: ColorMetric, p: Coords, q: Coords) -> f64 {
    let unit = |c: Rgb| [c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0];
    match (p, q) {
        (Coords::Srgb(a), Coords::Srgb(b)) => {
            let ([r0, g0, b0], [r1, g1, b1]) = (unit(a), unit(b));
            let (dr, dg, db) = (r0 - r1, g0 - g1, b0 - b1);
            match metric {
                ColorMetric::YCbCr => {
                    let cb = -0.1146 * dr - 0.3854 * dg + 0.5 * db;
                    let cr = 0.5 * dr - 0.4542 * dg - 0.0458 * db;
                    (cb * cb + cr * cr).sqrt() * std::f64::consts::SQRT_2
                }
                ColorMetric::Rgb => (dr * dr + dg * dg + db * db).sqrt() / 3f64.sqrt(),
                _ => dr.abs().max(dg.abs()).max(db.abs()),
            }
        }
        (Coords::Oklab(p), Coords::Oklab(q)) => ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt(),
        (Coords::Lab(p), Coords::Lab(q)) => ciede2000(p, q) / 100.0,
        _ => f64::INFINITY,
    }
}

/// 探す色・許容値・色差を決めた判定。探す色の変換は作るときに 1 回だけ行う。
#[derive(Debug, Clone, Copy)]
pub struct ColorMatcher {
    target: Rgb,
    tolerance: u8,
    metric: ColorMetric,
    target_coords: Coords,
    /// 色差の上限（`tolerance × step`）。丸めの誤差で境目の色が落ちないよう、わずかに広げる
    limit: f64,
}

impl ColorMatcher {
    pub fn new(target: Rgb, tolerance: u8, metric: ColorMetric) -> Self {
        Self {
            target,
            tolerance,
            metric,
            target_coords: coords(metric, target),
            limit: tolerance as f64 * metric.step() * (1.0 + 1e-9),
        }
    }

    pub fn matches(&self, c: Rgb) -> bool {
        if c == self.target {
            return true;
        }
        match self.metric {
            // 今まで（v0.3.1 の `within`）と同じ整数の判定
            ColorMetric::PerChannel => {
                let d = |x: u8, y: u8| x.abs_diff(y) <= self.tolerance;
                d(c.r, self.target.r) && d(c.g, self.target.g) && d(c.b, self.target.b)
            }
            // 0 はどの色差でも完全一致（YCbCr では明るさだけ違う色の差が 0 になるため）
            _ if self.tolerance == 0 => false,
            m => distance_of(m, coords(m, c), self.target_coords) <= self.limit,
        }
    }
}

/// 色相順の並べ替えに使う色空間。保存キーは `settings.hue_space`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HueSpace {
    /// 今まで（〜v0.3.1）
    #[default]
    Hsv,
    Oklch,
}

impl HueSpace {
    pub const ALL: [HueSpace; 2] = [HueSpace::Hsv, HueSpace::Oklch];

    pub fn label(self) -> &'static str {
        match self {
            HueSpace::Hsv => "HSV",
            HueSpace::Oklch => "Oklch",
        }
    }
}

/// 色相順の並べ替えの鍵 `(無彩色か, 色相または −明るさ, −明るさ)`。小さい順に並べる。
/// 色相が意味を持たない色（彩度が低い・暗い）は後ろにまとめ、明るい順に並べる（どちらの色空間でも同じ考え方）。
///
/// - HSV（今まで）: S < 0.12 か V < 0.08 を無彩色にする
/// - Oklch: C < 0.025 か L < 0.19 を無彩色にする。HSV の境目に合わせた値
///   （HSV の S = 0.12 の色の C の中央値がおよそ 0.026、V = 0.08 の灰色の L がおよそ 0.193）
pub fn hue_sort_key(c: Rgb, space: HueSpace) -> (bool, f32, f32) {
    let (achromatic, hue, light) = match space {
        HueSpace::Hsv => {
            let (h, s, v) = c.hsv();
            (s < 0.12 || v < 0.08, h, v)
        }
        HueSpace::Oklch => {
            let [l, ch, h] = oklch(c);
            (ch < 0.025 || l < 0.19, h as f32, l as f32)
        }
    };
    (achromatic, if achromatic { -light } else { hue }, -light)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(n: u32) -> Rgb {
        Rgb::new((n >> 16) as u8, (n >> 8) as u8, n as u8)
    }

    #[test]
    fn oklab_of_red_matches_css_color_4() {
        let [l, a, b] = oklab(rgb(0xff0000));
        assert!((l - 0.62796).abs() < 2e-5 && (a - 0.22486).abs() < 2e-5 && (b - 0.12585).abs() < 2e-5, "{l} {a} {b}");
        let [l, c, h] = oklch(rgb(0xff0000));
        assert!((l - 0.62796).abs() < 2e-5 && (c - 0.25768).abs() < 2e-5 && (h - 29.2339).abs() < 1e-3, "{l} {c} {h}");
        let [l, a, b] = oklab(rgb(0xffffff));
        assert!((l - 1.0).abs() < 1e-4 && a.abs() < 1e-4 && b.abs() < 1e-4);
        let [l, a, b] = oklab(rgb(0x000000));
        assert!(l.abs() < 1e-12 && a.abs() < 1e-12 && b.abs() < 1e-12);
    }

    #[test]
    fn lab_uses_d65_white() {
        let [l, a, b] = lab(rgb(0xffffff));
        assert!((l - 100.0).abs() < 1e-3 && a.abs() < 1e-3 && b.abs() < 1e-3, "{l} {a} {b}");
        // sRGB の赤（D65）: L 53.24, a 80.09, b 67.20
        let [l, a, b] = lab(rgb(0xff0000));
        assert!((l - 53.24).abs() < 0.01 && (a - 80.09).abs() < 0.01 && (b - 67.20).abs() < 0.01, "{l} {a} {b}");
    }

    /// Sharma, Wu, Dalal (2005) の試験データ（`verify_colorspace.py` と同じ 12 組）
    #[test]
    fn ciede2000_matches_sharma_test_data() {
        let pairs = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, -1.0, 2.0], [50.0, 0.0, 0.0], 2.3669),
            ([50.0, 2.4900, -0.0010], [50.0, -2.4900, 0.0009], 7.1792),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([50.0, 2.5, 0.0], [61.0, -5.0, 29.0], 22.8977),
            ([50.0, 2.5, 0.0], [56.0, -27.0, -3.0], 31.9030),
            ([50.0, 2.5, 0.0], [58.0, 24.0, 15.0], 19.4535),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([2.0776, 0.0795, -1.1350], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (p, q, want) in pairs {
            let got = ciede2000(p, q);
            assert!((got - want).abs() < 1e-4, "{p:?} {q:?}: {got} / {want}");
        }
    }

    /// 共有部品と同じ: 黒と白の差がおよそ 1、同じ色は 0、YCbCr は明るさだけの差を見ない
    #[test]
    fn distances_are_normalized_like_shared_parts() {
        for m in [ColorMetric::Rgb, ColorMetric::Oklab, ColorMetric::Ciede2000, ColorMetric::PerChannel] {
            let d = distance(m, rgb(0x000000), rgb(0xffffff));
            assert!((d - 1.0).abs() < 0.01, "{m:?}: {d}");
            assert_eq!(distance(m, rgb(0x4d9933), rgb(0x4d9933)), 0.0);
        }
        assert!(distance(ColorMetric::YCbCr, rgb(0x333333), rgb(0xcccccc)).abs() < 1e-9);
        assert!(distance(ColorMetric::YCbCr, rgb(0xff0000), rgb(0x00ffff)) > 0.5);
    }

    /// `ColorMetric::step` の値は、doc に書いた格子の各色で立方体の 8 つの角までの色差の最大を取った中央値（N = 1）
    #[test]
    fn metric_steps_cover_the_cube_corners() {
        let levels: Vec<i16> = (0..=17).map(|i| i * 15).collect();
        for (m, tol) in [(ColorMetric::Rgb, 1e-6), (ColorMetric::YCbCr, 1e-4), (ColorMetric::Oklab, 0.01), (ColorMetric::Ciede2000, 0.01)] {
            let mut worst = Vec::new();
            for &r in &levels {
                for &g in &levels {
                    for &b in &levels {
                        let mut far = -1.0f64;
                        for corner in 0..8 {
                            let s = |bit: i16| if corner & (1 << bit) != 0 { 1 } else { -1 };
                            let n = [r + s(0), g + s(1), b + s(2)];
                            if n.iter().any(|v| !(0..=255).contains(v)) {
                                continue;
                            }
                            let c0 = Rgb::new(r as u8, g as u8, b as u8);
                            let c1 = Rgb::new(n[0] as u8, n[1] as u8, n[2] as u8);
                            far = far.max(distance(m, c0, c1));
                        }
                        worst.push(far);
                    }
                }
            }
            worst.sort_by(f64::total_cmp);
            let median = (worst[worst.len() / 2] + worst[(worst.len() - 1) / 2]) * 0.5;
            assert!((median / m.step() - 1.0).abs() < tol, "{m:?}: 中央値 {median:.7} / step {:.7}", m.step());
        }
    }

    /// RGB と YCbCr は、今までの各成分の差で拾えた色をすべて拾う（立方体が球・円柱に収まる）
    #[test]
    fn rgb_and_ycbcr_cover_the_old_cube() {
        for n in [1u8, 2, 5, 16, 64] {
            for target in [rgb(0x808080), rgb(0xff0000), rgb(0x1a2a5e), rgb(0x000000)] {
                let old = ColorMatcher::new(target, n, ColorMetric::PerChannel);
                let rgb_m = ColorMatcher::new(target, n, ColorMetric::Rgb);
                let ycc = ColorMatcher::new(target, n, ColorMetric::YCbCr);
                let step = (n as usize / 4).max(1);
                for dr in (-(n as i16)..=n as i16).step_by(step) {
                    for dg in (-(n as i16)..=n as i16).step_by(step) {
                        for db in [-(n as i16), 0, n as i16] {
                            let v = [target.r as i16 + dr, target.g as i16 + dg, target.b as i16 + db];
                            if v.iter().any(|x| !(0..=255).contains(x)) {
                                continue;
                            }
                            let c = Rgb::new(v[0] as u8, v[1] as u8, v[2] as u8);
                            assert!(old.matches(c));
                            assert!(rgb_m.matches(c), "RGB {n} {} {}", target.hex(), c.hex());
                            assert!(ycc.matches(c), "YCbCr {n} {} {}", target.hex(), c.hex());
                        }
                    }
                }
            }
        }
        // 丸めで 1 ずつずれた色は、N = 1 なら RGB でも拾う
        assert!(ColorMatcher::new(rgb(0xff0000), 1, ColorMetric::Rgb).matches(rgb(0xfe0101)));
    }

    #[test]
    fn matcher_tolerance_zero_is_exact_for_every_metric() {
        for m in ColorMetric::ALL {
            let k = ColorMatcher::new(rgb(0x808080), 0, m);
            assert!(k.matches(rgb(0x808080)), "{m:?}");
            assert!(!k.matches(rgb(0x818181)), "{m:?}: 明るさだけ違う色も 0 では別");
            assert!(!k.matches(rgb(0x808081)), "{m:?}");
        }
    }

    #[test]
    fn matcher_rgb_is_the_circumscribed_sphere() {
        let k = ColorMatcher::new(rgb(0x808080), 4, ColorMetric::Rgb);
        assert!(k.matches(rgb(0x848484)), "3 成分 4 段（立方体の角）は入る");
        assert!(!k.matches(rgb(0x858484)));
        assert!(k.matches(rgb(0x868080)), "1 成分だけなら 4√3 ≒ 6.9 段まで入る");
        assert!(!k.matches(rgb(0x878080)));
    }

    #[test]
    fn matcher_ycbcr_ignores_brightness() {
        let k = ColorMatcher::new(rgb(0x808080), 1, ColorMetric::YCbCr);
        assert!(k.matches(rgb(0x202020)) && k.matches(rgb(0xffffff)));
        assert!(!k.matches(rgb(0x8080ff)));
    }

    #[test]
    fn hue_key_oklch_order_and_achromatic() {
        let mut colors = [rgb(0x0000ff), rgb(0x808080), rgb(0xffff00), rgb(0xff0000), rgb(0x00ff00), rgb(0xff00ff), rgb(0x00ffff)];
        colors.sort_by(|a, b| {
            let (ka, kb) = (hue_sort_key(*a, HueSpace::Oklch), hue_sort_key(*b, HueSpace::Oklch));
            ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1)).then(ka.2.total_cmp(&kb.2))
        });
        let hex: Vec<String> = colors.iter().map(|c| c.hex()).collect();
        // Oklch の色相: 赤 29°・黄 110°・緑 142°・シアン 195°・青 264°・マゼンタ 328°。灰色は最後
        assert_eq!(hex, ["ff0000", "ffff00", "00ff00", "00ffff", "0000ff", "ff00ff", "808080"]);
        assert!(hue_sort_key(rgb(0x101010), HueSpace::Oklch).0, "暗い色は無彩色");
        assert!(hue_sort_key(rgb(0xf5f0f0), HueSpace::Oklch).0, "彩度の低い色は無彩色");
        assert!(!hue_sort_key(rgb(0x000080), HueSpace::Oklch).0, "暗めでも鮮やかな紺は色相で並べる");
    }
}
