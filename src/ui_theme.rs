// 生成物: 正本は AI/tools/plugin_common/ui_theme.rs（開発用のリポジトリ）。ここを直さず、正本を直して
// `python AI/tools/sync_plugin_common.py` で写す（`--check` で同じかを確かめる）。
//! 自作プラグインの画面で使う色と大きさの名前付きの値（共通の規約 `~/.claude/rules/common-ui.md`「色と余白」「押せる所」）。
//!
//! 画面のコードからは名前で引き、16 進の色や画素の数を直に書かない。意味の名前（「エラーの文字」）で持ち、見た目を変えるときはここだけを直す。
//! そのプラグインだけの色（グラフの線など）は、拠点の `src/colors.rs` に名前を付けて置く。
//! 本体の配色（`aviutl2_eframe::aviutl2_visuals()`）は暗い背景なので、ここの色も暗い背景の上で読める明るさにしている。
//!
//! 写しは `python AI/tools/sync_plugin_common.py` が各拠点の `src/ui_theme.rs` へ置く。写しを直さず、ここを直す。

#![allow(dead_code)]

use aviutl2_eframe::egui::{self, Color32};

// ---- 文字の色（意味ごと） ----

/// エラー（失敗した・できない）の文字
pub const ERROR_TEXT: Color32 = Color32::from_rgb(235, 110, 110);
/// 警告（できたが気をつける・一部だけ）の文字
pub const WARN_TEXT: Color32 = Color32::from_rgb(235, 200, 90);
/// 成功（書き込めた・揃っている）の文字
pub const OK_TEXT: Color32 = Color32::from_rgb(120, 200, 120);
/// 補足・無効の弱い文字
pub const WEAK_TEXT: Color32 = Color32::from_gray(150);
/// 強調（選んでいる・今の対象）の文字
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(120, 180, 255);

// ---- 面の色 ----

/// 一覧の選んだ行の背景
pub const SELECTED_ROW_BG: Color32 = Color32::from_rgb(45, 70, 105);
/// 壊す操作（削除・上書き）のボタンの背景
pub const DANGER_BUTTON_BG: Color32 = Color32::from_rgb(120, 45, 45);

// ---- 大きさ ----

/// 押せる所の当たり判定の最小（PC の画面。WCAG 2.2 の 2.5.8）。見た目が小さい印・取っ手も、当たり判定はこれ以上にする
pub const MIN_HIT: f32 = 24.0;
/// 欄と欄の間（同じまとまりの中）
pub const GAP_ITEM: f32 = 4.0;
/// まとまりとまとまりの間
pub const GAP_SECTION: f32 = 10.0;
/// 壊す操作のボタンを、確定・実行のボタンから離す間（Fitts の法則。押し間違いを減らす）
pub const GAP_DANGER: f32 = 24.0;

/// 状態の行の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StatusKind {
    #[default]
    Info,
    Ok,
    Warn,
    Error,
}

impl StatusKind {
    pub fn color(self, ui: &egui::Ui) -> Color32 {
        match self {
            StatusKind::Info => ui.visuals().text_color(),
            StatusKind::Ok => OK_TEXT,
            StatusKind::Warn => WARN_TEXT,
            StatusKind::Error => ERROR_TEXT,
        }
    }
}

/// 状態の行（成否・エラー）を種類の色で出す。エラーは「何が起きたか・なぜか・次に何をすればよいか」を書いた文を渡す
pub fn status_label(ui: &mut egui::Ui, kind: StatusKind, text: impl Into<String>) -> egui::Response {
    let color = kind.color(ui);
    ui.add(egui::Label::new(egui::RichText::new(text.into()).color(color)).wrap())
}

/// 当たり判定が `MIN_HIT` 四方以上の小さなボタン（×・▲▼ などの印）
pub fn small_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(text).min_size(egui::vec2(MIN_HIT, MIN_HIT)))
}

/// 壊す操作のボタン（削除・上書き・一括）。背景で見分けられるようにし、確定のボタンの隣に置くときは前に `GAP_DANGER` を空ける
pub fn danger_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(text).fill(DANGER_BUTTON_BG).min_size(egui::vec2(MIN_HIT, MIN_HIT)))
}
