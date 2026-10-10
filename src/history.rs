//! 色履歴のデータと保存
//!
//! 本体のパレット（1 つ 64 色固定、`PALETTE_INFO::PALETTE_NUM`）は使わず、
//! プラグイン専用のファイルに保存する。件数の上限は設定値だけで決まる。
//!
//! 履歴は複数の「リスト」を持てる。最初は「デフォルト」だけで、利用者が作ったリストを
//! 選んでいる間は、記録した色がそのリストにだけ入る（v0.2.0〜）。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::color::{CopyFormat, Rgb};

/// 1: v0.1.0（単一の履歴） / 2: v0.2.0（リスト）
pub const FILE_VERSION: u32 = 2;
pub const MAX_ENTRIES_DEFAULT: usize = 500;
pub const MAX_ENTRIES_MIN: usize = 16;
pub const MAX_ENTRIES_MAX: usize = 10_000;
pub const DEFAULT_LIST_ID: &str = "default";
pub const DEFAULT_LIST_NAME: &str = "デフォルト";

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub color: Rgb,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub first_used: u64,
    #[serde(default)]
    pub last_used: u64,
    /// 最後に使われた場所（「テキスト / 文字色」など）。
    #[serde(default)]
    pub last_source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortMode {
    /// 最後に使った順
    #[default]
    Recent,
    /// 使った回数順
    Frequent,
    /// 色相順（無彩色は最後に明るい順）
    Hue,
}

impl SortMode {
    pub const ALL: [SortMode; 3] = [SortMode::Recent, SortMode::Frequent, SortMode::Hue];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Recent => "新しい順",
            SortMode::Frequent => "使用回数順",
            SortMode::Hue => "色相順",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// 色を変えたときに自動で記録する
    #[serde(default = "default_true")]
    pub auto_record: bool,
    /// 1 つのリストでピン留めしていない色の最大件数
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    /// クリックでコピーする形式
    #[serde(default, deserialize_with = "default_if_unknown")]
    pub copy_format: CopyFormat,
    #[serde(default, deserialize_with = "default_if_unknown")]
    pub sort: SortMode,
    /// 色見本 1 個の大きさ（px）
    #[serde(default = "default_swatch_size")]
    pub swatch_size: f32,
}

/// 列挙の項目を読む。知らない値（新しい版が足した並び順・コピー形式）は、その項目だけ初期値にする。
/// 列挙をそのまま読むと、知らない値 1 つでファイル全体が読めなくなり、履歴ごと退避される。
fn default_if_unknown<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match T::deserialize(&value) {
        Ok(v) => Ok(v),
        Err(e) => {
            tracing::warn!("ColorHistory_H: 設定の値 {value} を読めないので初期値にします（{e}）");
            Ok(T::default())
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_max_entries() -> usize {
    MAX_ENTRIES_DEFAULT
}
fn default_swatch_size() -> f32 {
    28.0
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_record: true,
            max_entries: MAX_ENTRIES_DEFAULT,
            copy_format: CopyFormat::Plain,
            sort: SortMode::Recent,
            swatch_size: default_swatch_size(),
        }
    }
}

/// 色のリスト 1 つ。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColorList {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl ColorList {
    pub fn new(id: &str, name: &str, now: u64) -> Self {
        Self { id: id.into(), name: name.into(), created: now, entries: Vec::new() }
    }

    pub fn is_default(&self) -> bool {
        self.id == DEFAULT_LIST_ID
    }

    fn index_of(&self, color: Rgb) -> Option<usize> {
        self.entries.iter().position(|e| e.color == color)
    }

    pub fn get(&self, color: Rgb) -> Option<&Entry> {
        self.entries.iter().find(|e| e.color == color)
    }

    /// 色を使ったことを記録する。既にあれば回数と時刻を更新する（重複は作らない）。
    pub fn record(&mut self, color: Rgb, source: &str, now: u64, max_entries: usize) {
        match self.index_of(color) {
            Some(i) => {
                let e = &mut self.entries[i];
                e.use_count = e.use_count.saturating_add(1);
                e.last_used = now;
                if !source.is_empty() {
                    e.last_source = source.to_string();
                }
            }
            None => self.entries.push(Entry {
                color,
                label: String::new(),
                pinned: false,
                use_count: 1,
                first_used: now,
                last_used: now,
                last_source: source.to_string(),
            }),
        }
        self.enforce_limit(max_entries);
    }

    /// 他のリストからエントリを持ち込む（ラベル・ピン留めも写す）。既にあれば回数を足す。
    pub fn merge_entry(&mut self, entry: &Entry, max_entries: usize) {
        match self.index_of(entry.color) {
            Some(i) => {
                let e = &mut self.entries[i];
                e.use_count = e.use_count.saturating_add(entry.use_count.max(1));
                e.last_used = e.last_used.max(entry.last_used);
                if e.label.is_empty() {
                    e.label = entry.label.clone();
                }
                e.pinned |= entry.pinned;
            }
            None => self.entries.push(entry.clone()),
        }
        self.enforce_limit(max_entries);
    }

    /// ピン留めしていない色が上限を超えたら、最後に使ったのが古いものから消す。
    pub fn enforce_limit(&mut self, max_entries: usize) {
        let max = max_entries.clamp(MAX_ENTRIES_MIN, MAX_ENTRIES_MAX);
        let unpinned = self.entries.iter().filter(|e| !e.pinned).count();
        if unpinned <= max {
            return;
        }
        let mut victims: Vec<(u64, Rgb)> = self
            .entries
            .iter()
            .filter(|e| !e.pinned)
            .map(|e| (e.last_used, e.color))
            .collect();
        victims.sort_by_key(|(t, _)| *t);
        let drop: Vec<Rgb> = victims.into_iter().take(unpinned - max).map(|(_, c)| c).collect();
        self.entries.retain(|e| e.pinned || !drop.contains(&e.color));
    }

    pub fn set_pinned(&mut self, color: Rgb, pinned: bool, max_entries: usize) -> bool {
        match self.index_of(color) {
            Some(i) => {
                self.entries[i].pinned = pinned;
                if !pinned {
                    self.enforce_limit(max_entries);
                }
                true
            }
            None => false,
        }
    }

    pub fn set_label(&mut self, color: Rgb, label: &str) -> bool {
        match self.index_of(color) {
            Some(i) => {
                self.entries[i].label = label.trim().to_string();
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, color: Rgb) -> Option<Entry> {
        self.index_of(color).map(|i| self.entries.remove(i))
    }

    /// ピン留めしていない色をすべて消す。消した件数。
    pub fn clear_unpinned(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.pinned);
        before - self.entries.len()
    }

    /// 表示用の並び。`(ピン留め, それ以外)` を返す。検索語はカラーコード（`#` 等の有無は問わない）かラベルの部分一致。
    pub fn view(&self, sort: SortMode, query: &str) -> (Vec<&Entry>, Vec<&Entry>) {
        let q = normalize_query(query);
        let mut items: Vec<&Entry> = self.entries.iter().filter(|e| matches_query(e, &q)).collect();
        sort_entries(&mut items, sort);
        items.into_iter().partition(|e| e.pinned)
    }
}

/// 保存ファイル全体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub lists: Vec<ColorList>,
    /// 記録先・表示中のリスト
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub settings: Settings,
    /// v0.1.0 の形式（トップレベルの entries）を読むためだけの欄。書き出さない
    #[serde(default, skip_serializing)]
    entries: Vec<Entry>,
}

impl Default for Store {
    fn default() -> Self {
        let mut s = Self {
            version: FILE_VERSION,
            lists: Vec::new(),
            active: DEFAULT_LIST_ID.into(),
            settings: Settings::default(),
            entries: Vec::new(),
        };
        s.normalize();
        s
    }
}

impl Store {
    /// 読み込み後の整合: 旧形式の移行、デフォルトの存在、active の妥当性。
    pub fn normalize(&mut self) {
        if !self.lists.iter().any(|l| l.is_default()) {
            let mut default = ColorList::new(DEFAULT_LIST_ID, DEFAULT_LIST_NAME, 0);
            default.entries = std::mem::take(&mut self.entries);
            self.lists.insert(0, default);
        } else if !self.entries.is_empty() {
            let legacy = std::mem::take(&mut self.entries);
            let max = self.settings.max_entries;
            let default = self.lists.iter_mut().find(|l| l.is_default()).expect("default exists");
            for e in &legacy {
                default.merge_entry(e, max);
            }
        }
        // デフォルトを常に先頭に置く
        if let Some(i) = self.lists.iter().position(|l| l.is_default()) {
            if i != 0 {
                let d = self.lists.remove(i);
                self.lists.insert(0, d);
            }
        }
        if !self.lists.iter().any(|l| l.id == self.active) {
            self.active = DEFAULT_LIST_ID.into();
        }
        let max = self.settings.max_entries;
        for l in &mut self.lists {
            l.enforce_limit(max);
        }
        self.version = FILE_VERSION;
    }

    pub fn active_list(&self) -> &ColorList {
        self.lists
            .iter()
            .find(|l| l.id == self.active)
            .unwrap_or(&self.lists[0])
    }

    pub fn active_list_mut(&mut self) -> &mut ColorList {
        let idx = self.lists.iter().position(|l| l.id == self.active).unwrap_or(0);
        &mut self.lists[idx]
    }

    pub fn list_mut(&mut self, id: &str) -> Option<&mut ColorList> {
        self.lists.iter_mut().find(|l| l.id == id)
    }

    /// 選択中のリストへ記録する（他のリストには入れない）。
    pub fn record(&mut self, color: Rgb, source: &str, now: u64) {
        let max = self.settings.max_entries;
        self.active_list_mut().record(color, source, now, max);
    }

    pub fn set_active(&mut self, id: &str) -> bool {
        if self.lists.iter().any(|l| l.id == id) {
            self.active = id.into();
            true
        } else {
            false
        }
    }

    /// 新しいリストを作って選択する。作ったリストの id。
    pub fn create_list(&mut self, name: &str, now: u64) -> String {
        let base = format!("list-{now}");
        let mut id = base.clone();
        let mut n = 1;
        while self.lists.iter().any(|l| l.id == id) {
            n += 1;
            id = format!("{base}-{n}");
        }
        let name = self.unique_name(name.trim());
        self.lists.push(ColorList::new(&id, &name, now));
        self.active = id.clone();
        id
    }

    fn unique_name(&self, name: &str) -> String {
        let base = if name.is_empty() { "新しいリスト" } else { name };
        if !self.lists.iter().any(|l| l.name == base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} ({n})"))
            .find(|candidate| !self.lists.iter().any(|l| &l.name == candidate))
            .expect("unbounded")
    }

    /// デフォルトの名前は変えない。
    pub fn rename_list(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || id == DEFAULT_LIST_ID {
            return false;
        }
        let taken = self.lists.iter().any(|l| l.id != id && l.name == name);
        let unique = if taken { self.unique_name(name) } else { name.to_string() };
        match self.list_mut(id) {
            Some(l) => {
                l.name = unique;
                true
            }
            None => false,
        }
    }

    /// デフォルトは消せない。消したリストを選んでいたらデフォルトに戻す。
    pub fn delete_list(&mut self, id: &str) -> Option<ColorList> {
        if id == DEFAULT_LIST_ID {
            return None;
        }
        let idx = self.lists.iter().position(|l| l.id == id)?;
        let removed = self.lists.remove(idx);
        if self.active == id {
            self.active = DEFAULT_LIST_ID.into();
        }
        Some(removed)
    }

    /// 別のリストへ色を写す（元のリストからは消さない）。
    pub fn copy_entry_to(&mut self, from_list: &str, color: Rgb, to_list: &str) -> bool {
        let Some(entry) = self
            .lists
            .iter()
            .find(|l| l.id == from_list)
            .and_then(|l| l.get(color))
            .cloned()
        else {
            return false;
        };
        let max = self.settings.max_entries;
        match self.list_mut(to_list) {
            Some(l) => {
                l.merge_entry(&entry, max);
                true
            }
            None => false,
        }
    }

    pub fn apply_limit(&mut self) {
        let max = self.settings.max_entries;
        for l in &mut self.lists {
            l.enforce_limit(max);
        }
    }
}

fn normalize_query(query: &str) -> String {
    let q = query.trim().to_lowercase();
    let q = q.strip_prefix("<#").map(|s| s.trim_end_matches('>')).unwrap_or(&q);
    let q = q.strip_prefix('#').unwrap_or(q);
    let q = q.strip_prefix("0x").unwrap_or(q);
    q.to_string()
}

fn matches_query(e: &Entry, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    if e.color.hex().contains(q) || e.label.to_lowercase().contains(q) {
        return true;
    }
    // "255,128,0" のような入力は色として一致を見る
    Rgb::parse(q).is_some_and(|c| c == e.color)
}

fn sort_entries(items: &mut [&Entry], sort: SortMode) {
    match sort {
        SortMode::Recent => items.sort_by(|a, b| b.last_used.cmp(&a.last_used)),
        SortMode::Frequent => items.sort_by(|a, b| {
            b.use_count
                .cmp(&a.use_count)
                .then(b.last_used.cmp(&a.last_used))
        }),
        SortMode::Hue => items.sort_by(|a, b| {
            let key = |e: &Entry| {
                let (h, s, v) = e.color.hsv();
                // 彩度の低い色は色相が意味を持たないので後ろにまとめ、明るい順に並べる
                let achromatic = s < 0.12 || v < 0.08;
                (achromatic, if achromatic { -v } else { h }, -v)
            };
            let (ka, kb) = (key(a), key(b));
            ka.0.cmp(&kb.0)
                .then(ka.1.total_cmp(&kb.1))
                .then(ka.2.total_cmp(&kb.2))
        }),
    }
}

/// `Plugin/ColorHistory_H/history.json`
pub fn default_path() -> PathBuf {
    aviutl2::config::app_data_path()
        .join("Plugin")
        .join("ColorHistory_H")
        .join("history.json")
}

/// 読み込みの結果。
#[derive(Debug)]
pub struct Loaded {
    pub store: Store,
    /// 利用者に知らせること（退避した・保存しない）
    pub warning: Option<String>,
    /// 読めず、退避もできなかった。この起動の間は保存しない（読めなかったファイルを空の履歴で上書きしない）
    pub save_blocked: bool,
}

/// 読み込む。ファイルが無ければ空。
///
/// 読めないとき（解析の失敗だけでなく、UTF-8 でない・ロック中などの I/O エラーも）は
/// `history.broken-{秒}.json` へ退避してから空で始める。退避もできなければ `save_blocked` を立てる
/// （ルール `au2-plugin-checklist`「設定ファイルの下位互換」）。
pub fn load(path: &Path) -> Loaded {
    let fresh = |warning: Option<String>, save_blocked: bool| Loaded {
        store: Store::default(),
        warning,
        save_blocked,
    };
    let problem = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Store>(&text) {
            Ok(mut store) => {
                store.normalize();
                return Loaded { store, warning: None, save_blocked: false };
            }
            Err(e) => format!("解析できません: {e}"),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh(None, false),
        Err(e) => format!("読めません: {e}"),
    };
    let backup = backup_path(path);
    match std::fs::rename(path, &backup) {
        Ok(()) => fresh(
            Some(format!(
                "履歴ファイルを読めなかったので退避し、空の履歴で始めます: {}（{problem}）",
                backup.display()
            )),
            false,
        ),
        Err(e) => fresh(
            Some(format!(
                "履歴ファイルを読めず、退避もできなかったので、この起動の間は履歴を保存しません: {}（{problem} / 退避: {e}）",
                path.display()
            )),
            true,
        ),
    }
}

/// 読めなかった履歴の退避先（`history.broken-{秒}.json`。v0.2.3 までと同じ名前）。
/// 同じ秒に退避したものがあれば番号を足す（退避済みのファイルを上書きしない）。
fn backup_path(path: &Path) -> PathBuf {
    let secs = now_secs();
    let first = path.with_extension(format!("broken-{secs}.json"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| path.with_extension(format!("broken-{secs}-{n}.json")))
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// 一時ファイルに書いてから置き換える（書き込み途中で落ちても元のファイルを壊さない）。
pub fn save(path: &Path, store: &Store) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(n: u32) -> Rgb {
        Rgb::new((n >> 16) as u8, (n >> 8) as u8, n as u8)
    }

    #[test]
    fn record_deduplicates_and_counts() {
        let mut l = ColorList::new("x", "x", 0);
        l.record(rgb(0xff0000), "テキスト / 文字色", 10, 500);
        l.record(rgb(0x00ff00), "図形 / 色", 20, 500);
        l.record(rgb(0xff0000), "縁取り / 縁色", 30, 500);
        assert_eq!(l.entries.len(), 2);
        let red = l.get(rgb(0xff0000)).unwrap();
        assert_eq!(red.use_count, 2);
        assert_eq!(red.first_used, 10);
        assert_eq!(red.last_used, 30);
        assert_eq!(red.last_source, "縁取り / 縁色");
    }

    #[test]
    fn limit_drops_oldest_unpinned_but_keeps_pins() {
        let max = MAX_ENTRIES_MIN;
        let mut l = ColorList::new("x", "x", 0);
        l.record(rgb(1), "", 1, max);
        l.set_pinned(rgb(1), true, max);
        for i in 0..max as u32 + 5 {
            l.record(rgb(100 + i), "", 10 + i as u64, max);
        }
        assert_eq!(l.entries.iter().filter(|e| !e.pinned).count(), max);
        assert!(l.get(rgb(1)).is_some(), "ピン留めは上限で消えない");
        assert!(l.get(rgb(100)).is_none(), "一番古い色から消える");
    }

    #[test]
    fn view_splits_pins_and_filters() {
        let mut l = ColorList::new("x", "x", 0);
        l.record(rgb(0xff8000), "", 1, 500);
        l.record(rgb(0x0080ff), "", 2, 500);
        l.set_pinned(rgb(0xff8000), true, 500);
        l.set_label(rgb(0x0080ff), "  空の青  ");
        let (pins, rest) = l.view(SortMode::Recent, "");
        assert_eq!((pins.len(), rest.len()), (1, 1));
        assert_eq!(rest[0].label, "空の青");
        assert_eq!(l.view(SortMode::Recent, "#FF80").0.len(), 1);
        assert_eq!(l.view(SortMode::Recent, "空").1.len(), 1);
        assert_eq!(l.view(SortMode::Recent, "0,128,255").1.len(), 1);
    }

    #[test]
    fn sort_modes() {
        let mut l = ColorList::new("x", "x", 0);
        l.record(rgb(0x0000ff), "", 1, 500);
        l.record(rgb(0xff0000), "", 2, 500);
        l.record(rgb(0x808080), "", 3, 500);
        l.record(rgb(0x00ff00), "", 4, 500);
        l.record(rgb(0x0000ff), "", 5, 500);
        l.record(rgb(0x0000ff), "", 6, 500);
        let hexes = |v: Vec<&Entry>| v.iter().map(|e| e.color.hex()).collect::<Vec<_>>();
        assert_eq!(hexes(l.view(SortMode::Recent, "").1), ["0000ff", "00ff00", "808080", "ff0000"]);
        assert_eq!(hexes(l.view(SortMode::Frequent, "").1)[0], "0000ff");
        assert_eq!(hexes(l.view(SortMode::Hue, "").1), ["ff0000", "00ff00", "0000ff", "808080"]);
    }

    #[test]
    fn records_go_only_to_the_active_list() {
        let mut s = Store::default();
        s.record(rgb(0x111111), "", 1);
        let id = s.create_list("案件A", 100);
        assert_eq!(s.active, id, "作ったリストが選択される");
        s.record(rgb(0x222222), "", 2);
        let default = &s.lists[0];
        assert!(default.get(rgb(0x111111)).is_some());
        assert!(default.get(rgb(0x222222)).is_none(), "デフォルトには入れない");
        assert!(s.active_list().get(rgb(0x222222)).is_some());
    }

    #[test]
    fn list_management() {
        let mut s = Store::default();
        let a = s.create_list("案件", 1);
        let b = s.create_list("案件", 1);
        assert_ne!(a, b, "同じ時刻でも id は重複しない");
        assert_eq!(s.lists.iter().filter(|l| l.name.starts_with("案件")).count(), 2);
        assert!(s.lists.iter().any(|l| l.name == "案件 (2)"));
        assert!(!s.rename_list(DEFAULT_LIST_ID, "全体"), "デフォルトは改名しない");
        assert!(s.rename_list(&a, "MV"));
        assert!(s.delete_list(DEFAULT_LIST_ID).is_none(), "デフォルトは消せない");
        s.set_active(&b);
        assert!(s.delete_list(&b).is_some());
        assert_eq!(s.active, DEFAULT_LIST_ID, "選択中を消したらデフォルトへ戻る");
    }

    #[test]
    fn copy_entry_between_lists_keeps_label_and_pin() {
        let mut s = Store::default();
        s.record(rgb(0xabcdef), "", 1);
        s.lists[0].set_label(rgb(0xabcdef), "主役");
        s.lists[0].set_pinned(rgb(0xabcdef), true, 500);
        let id = s.create_list("B", 2);
        assert!(s.copy_entry_to(DEFAULT_LIST_ID, rgb(0xabcdef), &id));
        let copied = s.active_list().get(rgb(0xabcdef)).unwrap();
        assert_eq!(copied.label, "主役");
        assert!(copied.pinned);
        assert!(s.lists[0].get(rgb(0xabcdef)).is_some(), "元からは消さない");
    }

    /// v0.1.0 の保存ファイル（トップレベルの entries）はデフォルトへ移行する。
    #[test]
    fn migrates_v1_file() {
        let v1 = r#"{"version":1,"entries":[{"color":"ff0000","label":"赤","pinned":true,"use_count":3,"first_used":1,"last_used":2,"last_source":""}],"settings":{"auto_record":false,"max_entries":300,"copy_format":"Hash","sort":"Hue","swatch_size":32.0}}"#;
        let mut s: Store = serde_json::from_str(v1).unwrap();
        s.normalize();
        assert_eq!(s.version, FILE_VERSION);
        assert_eq!(s.lists.len(), 1);
        assert_eq!(s.active, DEFAULT_LIST_ID);
        let e = s.lists[0].get(rgb(0xff0000)).unwrap();
        assert_eq!((e.label.as_str(), e.pinned, e.use_count), ("赤", true, 3));
        assert!(!s.settings.auto_record);
        // 書き出しに旧形式の欄は残さない
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert!(v.get("entries").is_none());
        assert!(v.get("lists").is_some());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("colorhistory_h_test_{}", now_secs()));
        let path = dir.join("history.json");
        let mut s = Store::default();
        s.record(rgb(0x123456), "テスト", 42);
        s.create_list("案件", 50);
        s.record(rgb(0x654321), "", 60);
        s.settings.copy_format = CopyFormat::TextTag;
        save(&path, &s).unwrap();
        assert!(!path.with_extension("json.tmp").exists(), "一時ファイルは置き換えで消える");
        let loaded = load(&path);
        assert!(loaded.warning.is_none());
        assert!(!loaded.save_blocked);
        assert_eq!(loaded.store, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colorhistory_h_{name}_{}_{}", std::process::id(), now_secs()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_starts_empty_and_can_save() {
        let dir = test_dir("missing");
        let loaded = load(&dir.join("history.json"));
        assert!(loaded.warning.is_none());
        assert!(!loaded.save_blocked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_is_backed_up_not_overwritten() {
        let dir = test_dir("broken");
        let path = dir.join("history.json");
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = load(&path);
        assert!(loaded.store.lists[0].entries.is_empty());
        assert!(loaded.warning.is_some());
        assert!(!loaded.save_blocked, "退避できたら保存してよい");
        assert!(!path.exists(), "壊れたファイルは退避されている");
        let backups: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// UTF-8 でないファイル（解析より前の I/O エラー）も、解析の失敗と同じに退避する
    #[test]
    fn non_utf8_file_is_backed_up() {
        let dir = test_dir("nonutf8");
        let path = dir.join("history.json");
        let bytes = [0x7b, 0x22, 0x82, 0xa0, 0x22, 0x7d]; // {"あ"} の CP932
        std::fs::write(&path, bytes).unwrap();
        let loaded = load(&path);
        assert!(loaded.warning.is_some());
        assert!(!loaded.save_blocked);
        assert!(!path.exists());
        let backups: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), bytes, "中身はそのまま");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同じ秒に 2 回退避しても、先の退避を上書きしない
    #[test]
    fn backup_does_not_overwrite_earlier_backup() {
        let dir = test_dir("twice");
        let path = dir.join("history.json");
        std::fs::write(&path, "{ first").unwrap();
        let _ = load(&path);
        std::fs::write(&path, "{ second").unwrap();
        let _ = load(&path);
        let mut contents: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        contents.sort();
        assert_eq!(contents, ["{ first", "{ second"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 他のプロセスが共有なしで開いている（読めず、退避の rename もできない）なら、保存しない
    #[cfg(windows)]
    #[test]
    fn locked_file_blocks_saving() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = test_dir("locked");
        let path = dir.join("history.json");
        std::fs::write(&path, r#"{"version":2,"lists":[]}"#).unwrap();
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        let loaded = load(&path);
        assert!(loaded.save_blocked, "退避できなければ保存しない");
        assert!(loaded.warning.is_some());
        drop(lock);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"version":2,"lists":[]}"#,
            "元のファイルは残っている"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 新しい版が足した並び順・コピー形式は、その項目だけ初期値にし、ほかの設定と履歴は読む
    #[test]
    fn unknown_enum_values_fall_back_per_field() {
        let newer = r#"{"version":2,"lists":[{"id":"default","name":"デフォルト","entries":[{"color":"ff0000"}]}],
            "active":"default","future_key":1,
            "settings":{"auto_record":false,"max_entries":300,"copy_format":"Css","sort":{"Saturation":1},"swatch_size":32.0}}"#;
        let mut s: Store = serde_json::from_str(newer).unwrap();
        s.normalize();
        assert_eq!(s.settings.copy_format, CopyFormat::default());
        assert_eq!(s.settings.sort, SortMode::default());
        assert!(!s.settings.auto_record);
        assert_eq!(s.settings.max_entries, 300);
        assert_eq!(s.settings.swatch_size, 32.0);
        assert!(s.lists[0].get(rgb(0xff0000)).is_some());
        // 知っている値はそのまま読む・欄が無ければ初期値
        let known: Settings = serde_json::from_str(r#"{"copy_format":"Lua","sort":"Hue"}"#).unwrap();
        assert_eq!((known.copy_format, known.sort), (CopyFormat::Lua, SortMode::Hue));
        let missing: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(missing, Settings::default());
    }
}
