//! ColorHistory_H の画面だけで使う色の表（意味の名前で持つ。共通の色は `ui_theme.rs`）。
//! 履歴の色そのもの（利用者が記録した色）は `Rgb` のデータで、ここには置かない。

#![allow(dead_code)]

use aviutl2_eframe::egui::Color32;

use crate::color::Rgb;

/// 履歴の色（データ）を画面の色へ
pub fn from_data(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// 色の無い見本（スポイトで読めなかったとき）の枠
pub const SWATCH_EMPTY_STROKE: Color32 = Color32::from_gray(90);
/// 選んでいる見本の枠
pub const SWATCH_SELECTED_STROKE: Color32 = Color32::from_rgb(255, 200, 80);
/// マウスが乗っている見本の枠
pub const SWATCH_HOVER_STROKE: Color32 = Color32::from_gray(210);
/// 通常の見本の枠
pub const SWATCH_STROKE: Color32 = Color32::from_gray(70);
/// 明るい見本の上のピン・ラベルの印
pub const MARK_ON_LIGHT: Color32 = Color32::from_gray(20);
/// 暗い見本の上のピン・ラベルの印
pub const MARK_ON_DARK: Color32 = Color32::from_gray(235);
