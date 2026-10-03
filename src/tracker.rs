//! 「色を変えたときだけ記録する」ための判定
//!
//! - フォーカス中のオブジェクトの色項目を毎回読み、**同じオブジェクトで値が変わった項目だけ**を候補にする
//! - 選び直した（フォーカスが変わった）ときは基準を取り直すだけで、記録しない
//! - 色設定ウィンドウでドラッグしている間は値が連続して変わるので、候補は
//!   **値が一定時間変わらず、マウスボタンが離れてから**確定する（途中の色で履歴を埋めない）
//!
//! 実機に依存しない純粋なロジックだけを置く（読み取りは `edit_ops`、スレッドは `watcher`）。

use std::collections::HashMap;

use crate::color::Rgb;

/// 色項目の住所。`effect_index` は同じ名前のエフェクトの何番目か（0 始まり）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColorItemKey {
    pub effect_name: String,
    pub effect_index: usize,
    pub item_name: String,
}

impl ColorItemKey {
    /// 表示用（「テキスト / 文字色」「縁取り:1 / 縁色」）。
    pub fn label(&self) -> String {
        let effect = self.effect_name.split('@').next().unwrap_or(&self.effect_name);
        if self.effect_index == 0 {
            format!("{effect} / {}", self.item_name)
        } else {
            format!("{effect}:{} / {}", self.effect_index, self.item_name)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorItem {
    pub key: ColorItemKey,
    /// 透明色（空値）は `None`
    pub value: Option<Rgb>,
}

/// フォーカス中オブジェクトの識別。オブジェクトのハンドルそのものは比較用の数値で持つ。
pub type ObjectId = u64;

#[derive(Debug, Clone)]
struct Pending {
    color: Rgb,
    source: String,
    changed_at_ms: u64,
}

#[derive(Debug, Default)]
pub struct ChangeTracker {
    object: Option<ObjectId>,
    values: HashMap<ColorItemKey, Option<Rgb>>,
    pending: HashMap<ColorItemKey, Pending>,
}

/// 値が落ち着いたとみなすまでの時間。
pub const SETTLE_MS: u64 = 500;

impl ChangeTracker {
    /// 読み取った結果を渡す。値の変わった項目は保留に入る（まだ記録しない）。
    pub fn observe(&mut self, object: Option<ObjectId>, items: &[ColorItem], now_ms: u64) {
        if object != self.object {
            // 選び直し: 基準を取り直すだけ。前のオブジェクトの保留は、そのオブジェクトで確定した色なので残す
            self.object = object;
            self.values = items.iter().map(|i| (i.key.clone(), i.value)).collect();
            return;
        }
        for item in items {
            match self.values.get(&item.key) {
                Some(old) if *old != item.value => {
                    if let Some(color) = item.value {
                        self.pending.insert(
                            item.key.clone(),
                            Pending { color, source: item.key.label(), changed_at_ms: now_ms },
                        );
                    }
                }
                // 新しく現れた項目（エフェクトの追加など）は基準に入れるだけ
                _ => {}
            }
        }
        self.values = items.iter().map(|i| (i.key.clone(), i.value)).collect();
    }

    /// 落ち着いた保留を取り出す。`buttons_down` の間は確定しない。
    pub fn take_settled(&mut self, now_ms: u64, buttons_down: bool) -> Vec<(Rgb, String)> {
        if buttons_down {
            return Vec::new();
        }
        let ready: Vec<ColorItemKey> = self
            .pending
            .iter()
            .filter(|(_, p)| now_ms.saturating_sub(p.changed_at_ms) >= SETTLE_MS)
            .map(|(k, _)| k.clone())
            .collect();
        let mut out = Vec::new();
        for key in ready {
            if let Some(p) = self.pending.remove(&key) {
                out.push((p.color, p.source));
            }
        }
        out
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(effect: &str, item: &str) -> ColorItemKey {
        ColorItemKey { effect_name: effect.into(), effect_index: 0, item_name: item.into() }
    }

    fn item(effect: &str, name: &str, hex: Option<u32>) -> ColorItem {
        ColorItem {
            key: key(effect, name),
            value: hex.map(|n| Rgb::new((n >> 16) as u8, (n >> 8) as u8, n as u8)),
        }
    }

    #[test]
    fn focusing_an_object_records_nothing() {
        let mut t = ChangeTracker::default();
        t.observe(Some(1), &[item("テキスト", "文字色", Some(0xffffff))], 0);
        t.observe(Some(2), &[item("図形", "色", Some(0xff0000))], 10);
        assert!(!t.has_pending());
        assert!(t.take_settled(10_000, false).is_empty());
    }

    #[test]
    fn changed_value_is_recorded_after_settling() {
        let mut t = ChangeTracker::default();
        t.observe(Some(1), &[item("テキスト", "文字色", Some(0xffffff))], 0);
        t.observe(Some(1), &[item("テキスト", "文字色", Some(0x123456))], 100);
        assert!(t.take_settled(300, false).is_empty(), "落ち着く前は確定しない");
        let got = t.take_settled(100 + SETTLE_MS, false);
        assert_eq!(got, vec![(Rgb::new(0x12, 0x34, 0x56), "テキスト / 文字色".to_string())]);
        assert!(!t.has_pending());
    }

    #[test]
    fn dragging_a_picker_records_only_the_final_color() {
        let mut t = ChangeTracker::default();
        t.observe(Some(1), &[item("図形", "色", Some(0x000000))], 0);
        for (i, c) in [0x100000, 0x200000, 0x300000, 0x400000].iter().enumerate() {
            t.observe(Some(1), &[item("図形", "色", Some(*c))], 50 * (i as u64 + 1));
        }
        // ボタンを押している間は、時間が経っても確定しない
        assert!(t.take_settled(5_000, true).is_empty());
        let got = t.take_settled(5_000, false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Rgb::new(0x40, 0, 0));
    }

    #[test]
    fn transparent_and_new_items_are_ignored() {
        let mut t = ChangeTracker::default();
        t.observe(Some(1), &[item("図形", "色", Some(0xffffff))], 0);
        // 透明にした
        t.observe(Some(1), &[item("図形", "色", None)], 10);
        // エフェクトを足した（新しい項目が現れた）
        t.observe(
            Some(1),
            &[item("図形", "色", None), item("縁取り", "縁色", Some(0x000000))],
            20,
        );
        assert!(t.take_settled(10_000, false).is_empty());
    }

    #[test]
    fn pending_of_previous_object_survives_refocus() {
        let mut t = ChangeTracker::default();
        t.observe(Some(1), &[item("図形", "色", Some(0x000000))], 0);
        t.observe(Some(1), &[item("図形", "色", Some(0xabcdef))], 10);
        // 確定前に別のオブジェクトを選んだ
        t.observe(Some(2), &[item("テキスト", "文字色", Some(0xffffff))], 20);
        let got = t.take_settled(10 + SETTLE_MS, false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Rgb::new(0xab, 0xcd, 0xef));
    }

    #[test]
    fn key_label() {
        assert_eq!(key("縁取り@Basic_S", "縁色").label(), "縁取り / 縁色");
        let k = ColorItemKey { effect_name: "グロー".into(), effect_index: 1, item_name: "光色".into() };
        assert_eq!(k.label(), "グロー:1 / 光色");
    }
}
