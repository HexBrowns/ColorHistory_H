//! 1 行の入力欄の確定と取り消し（ルール au2-rs-plugin「入力の確定と取り消し」）
//!
//! - 確定: Enter か、ほかをクリックしてフォーカスが外れたとき
//! - 取り消し: 入力中の Esc で、入力を始める前の文字に戻す
//!
//! 参照実装は MidpointTable_H の `number_text`。

use aviutl2_eframe::egui;

/// 1 行の入力欄の、このフレームでの終わり方
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnd {
    /// 入力中、または触っていない
    Editing,
    /// Enter か、フォーカスが外れて確定した。`changed` は入力を始める前と文字が違うか
    Committed { enter: bool, changed: bool },
    /// Esc で入力を始める前の文字に戻した
    Reverted,
}

impl LineEnd {
    /// Enter で確定した
    pub fn enter(self) -> bool {
        matches!(self, Self::Committed { enter: true, .. })
    }
}

/// フォーカスが外れたフレームの終わり方を決め、Esc なら `before` に戻す。
/// `before` が無い（フォーカスを得たフレームを見ていない）ときは、Esc でも今の文字のまま
fn finish(text: &mut String, before: Option<String>, escape: bool, enter: bool) -> LineEnd {
    if escape {
        if let Some(b) = before {
            *text = b;
        }
        return LineEnd::Reverted;
    }
    let changed = before.as_deref() != Some(text.as_str());
    LineEnd::Committed { enter, changed }
}

/// `ui.add(egui::TextEdit::singleline(text))` の応答の直後に呼ぶ。
/// フォーカスを得たときに元の文字を覚え、外れたときに Esc なら元に戻す。
/// egui はフレームの始めに Esc・Enter でフォーカスを外すので、`lost_focus()` のフレームのキーで見分ける
pub fn track(ui: &egui::Ui, resp: &egui::Response, text: &mut String) -> LineEnd {
    let key = resp.id.with("before_edit");
    if resp.gained_focus() {
        ui.data_mut(|d| d.insert_temp(key, text.clone()));
    }
    if !resp.lost_focus() {
        return LineEnd::Editing;
    }
    let before = ui.data_mut(|d| d.remove_temp::<String>(key));
    let (escape, enter) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Enter)));
    finish(text, before, escape, enter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_reverts_to_text_before_edit() {
        let mut t = "ff8000x".to_string();
        assert_eq!(finish(&mut t, Some("ff8000".into()), true, false), LineEnd::Reverted);
        assert_eq!(t, "ff8000");
    }

    #[test]
    fn escape_wins_over_enter() {
        let mut t = "abc".to_string();
        assert_eq!(finish(&mut t, Some("a".into()), true, true), LineEnd::Reverted);
        assert_eq!(t, "a");
    }

    #[test]
    fn escape_without_memory_keeps_text() {
        let mut t = "abc".to_string();
        assert_eq!(finish(&mut t, None, true, false), LineEnd::Reverted);
        assert_eq!(t, "abc");
    }

    #[test]
    fn enter_commits_and_reports_change() {
        let mut t = "new".to_string();
        let end = finish(&mut t, Some("old".into()), false, true);
        assert_eq!(end, LineEnd::Committed { enter: true, changed: true });
        assert!(end.enter());
        assert_eq!(t, "new");
    }

    #[test]
    fn blur_commits_without_enter() {
        let mut t = "same".to_string();
        let end = finish(&mut t, Some("same".into()), false, false);
        assert_eq!(end, LineEnd::Committed { enter: false, changed: false });
        assert!(!end.enter());
    }

    #[test]
    fn commit_without_memory_counts_as_changed() {
        let mut t = "x".to_string();
        assert_eq!(finish(&mut t, None, false, false), LineEnd::Committed { enter: false, changed: true });
    }
}
