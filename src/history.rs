//! 色履歴のデータと保存
//!
//! 本体のパレット（1 つ 64 色固定、`PALETTE_INFO::PALETTE_NUM`）は使わず、
//! プラグイン専用のファイルに保存する。件数の上限は設定値だけで決まる。
//!
//! 履歴は複数の「リスト」を持てる。最初は「デフォルト」だけで、利用者が作ったリストを
//! 選んでいる間は、記録した色がそのリストにだけ入る（v0.2.0〜）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::color::{CopyFormat, Rgb};
use crate::colorspace::{hue_sort_key, ColorMetric, HueSpace};

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
    /// 色相順の色空間（v0.4.0〜。無ければ今までの HSV）
    #[serde(default, deserialize_with = "default_if_unknown")]
    pub hue_space: HueSpace,
    /// 一括置換の色差（v0.4.0〜。無ければ今までの各成分の差）
    #[serde(default, deserialize_with = "default_if_unknown")]
    pub replace_metric: ColorMetric,
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
            hue_space: HueSpace::Hsv,
            replace_metric: ColorMetric::PerChannel,
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
    /// `hue_space` は色相順のときだけ使う。
    pub fn view(&self, sort: SortMode, hue_space: HueSpace, query: &str) -> (Vec<&Entry>, Vec<&Entry>) {
        let q = normalize_query(query);
        let mut items: Vec<&Entry> = self.entries.iter().filter(|e| matches_query(e, &q)).collect();
        sort_entries(&mut items, sort, hue_space);
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
    /// プロジェクトごとに最後に選んだリスト（`.aup2` のパスの鍵 → リストの id）。デフォルトを選んだプロジェクトは載せない（v0.5.0〜）。
    /// 載っていないプロジェクト（新規・知らないもの）を開いたら、記録先をデフォルトにする（ユーザーの指示、2026-10-24）
    #[serde(default)]
    pub project_lists: BTreeMap<String, String>,
    /// 開いているプロジェクトの鍵（未保存なら None）。書き出さない
    #[serde(skip)]
    current_project: Option<String>,
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
            project_lists: BTreeMap::new(),
            current_project: None,
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
            self.remember_project_list();
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
        self.remember_project_list();
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
        self.project_lists.retain(|_, l| l != id);
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

    /// プロジェクトを開いた。記録先を次の順で決める（ユーザーの ToDo「新規や知らないプロジェクトは既定のリスト」）:
    /// 1. このプロジェクトで最後に選んだリスト（デフォルトを選んだことも覚えている）
    /// 2. 名前がプロジェクトのファイル名かフォルダ名の頭と合うリスト（「マリンブルー・マジック♡」のリストと `マリンブルー・マジック♡_New.aup2`。
    ///    いちばん長く合うもの）。合ったらそのプロジェクトの記録先として覚える（選ばずに開いただけで切り替わらなかった。2026-10-24）
    /// 3. どれも無ければ（新規・知らないプロジェクト・消したリスト）デフォルト
    pub fn open_project(&mut self, path: Option<&Path>) -> OpenOutcome {
        self.current_project = path.map(project_key);
        let remembered = self
            .current_project
            .as_ref()
            .and_then(|k| self.project_lists.get(k))
            .filter(|id| self.lists.iter().any(|l| &l.id == *id))
            .cloned();
        let (want, reason) = match (remembered, path.and_then(|p| self.list_matching_project(p))) {
            (Some(id), _) => (id, OpenReason::Remembered),
            (None, Some(id)) => (id, OpenReason::NameMatched),
            (None, None) => (DEFAULT_LIST_ID.to_string(), if path.is_some() { OpenReason::Unknown } else { OpenReason::Unsaved }),
        };
        let changed = self.active != want;
        self.active = want;
        if reason == OpenReason::NameMatched {
            self.remember_project_list();
        }
        OpenOutcome { changed, reason }
    }

    /// 名前がプロジェクトのファイル名かフォルダ名の頭と合うリスト（デフォルトは除く。比べる前に全角と半角・大文字と小文字をそろえる）
    fn list_matching_project(&self, path: &Path) -> Option<String> {
        let norm = |s: &str| crate::egui_input::normalize_for_search(s.trim());
        let names: Vec<String> = [path.file_stem(), path.parent().and_then(|d| d.file_name())]
            .into_iter()
            .flatten()
            .map(|n| norm(&n.to_string_lossy()))
            .filter(|n| !n.is_empty())
            .collect();
        self.lists
            .iter()
            .filter(|l| !l.is_default())
            .filter_map(|l| {
                let ln = norm(&l.name);
                (!ln.is_empty() && names.iter().any(|n| n.starts_with(&ln))).then(|| (ln.chars().count(), l.id.clone()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, id)| id)
    }

    /// プロジェクトを保存した。新規を初めて保存した・別名で保存したときは、今の記録先をその名前で覚える。覚えを変えたら true
    pub fn project_saved(&mut self, path: &Path) -> bool {
        let key = project_key(path);
        if self.current_project.as_deref() == Some(key.as_str()) {
            return false;
        }
        self.current_project = Some(key);
        self.remember_project_list();
        true
    }

    /// 開いているプロジェクトの記録先として今のリストを覚える（未保存のプロジェクトでは覚えない）。
    /// デフォルトを選んだことも覚える（覚えないと、次に開いたとき名前の合うリストへ戻ってしまう）
    fn remember_project_list(&mut self) {
        let Some(k) = self.current_project.clone() else { return };
        self.project_lists.insert(k, self.active.clone());
    }

    pub fn apply_limit(&mut self) {
        let max = self.settings.max_entries;
        for l in &mut self.lists {
            l.enforce_limit(max);
        }
    }
}

/// プロジェクトを開いたときの記録先の決まり方（ログと画面に出す）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenReason {
    /// このプロジェクトで最後に選んだリスト
    Remembered,
    /// 名前がプロジェクトの名前と合うリスト
    NameMatched,
    /// 知らないプロジェクト → デフォルト
    Unknown,
    /// 未保存（新規）→ デフォルト
    Unsaved,
}

impl OpenReason {
    pub fn text(self) -> &'static str {
        match self {
            OpenReason::Remembered => "このプロジェクトで最後に選んだリスト",
            OpenReason::NameMatched => "名前がプロジェクトと合うリスト",
            OpenReason::Unknown => "初めてのプロジェクトなのでデフォルト",
            OpenReason::Unsaved => "未保存のプロジェクトなのでデフォルト",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOutcome {
    /// 記録先が変わった
    pub changed: bool,
    pub reason: OpenReason,
}

/// プロジェクトの覚えの鍵（Windows のパスは大文字と小文字を区別しないので小文字にし、区切りを `\` にそろえる）
pub fn project_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// 検索語を、項目と同じ形（全角と半角・大文字と小文字・ひらがなとカタカナ）にそろえてから、カラーコードの飾りを外す
fn normalize_query(query: &str) -> String {
    let q = crate::egui_input::normalize_for_search(query.trim());
    let q = q.strip_prefix("<#").map(|s| s.trim_end_matches('>')).unwrap_or(&q);
    let q = q.strip_prefix('#').unwrap_or(q);
    let q = q.strip_prefix("0x").unwrap_or(q);
    q.to_string()
}

fn matches_query(e: &Entry, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    if e.color.hex().contains(q) || crate::egui_input::normalize_for_search(&e.label).contains(q) {
        return true;
    }
    // "255,128,0" のような入力は色として一致を見る
    Rgb::parse(q).is_some_and(|c| c == e.color)
}

fn sort_entries(items: &mut Vec<&Entry>, sort: SortMode, hue_space: HueSpace) {
    match sort {
        SortMode::Recent => items.sort_by(|a, b| b.last_used.cmp(&a.last_used)),
        SortMode::Frequent => items.sort_by(|a, b| {
            b.use_count
                .cmp(&a.use_count)
                .then(b.last_used.cmp(&a.last_used))
        }),
        SortMode::Hue => {
            // 鍵は色ごとに 1 回だけ計算する（比較のたびに変換しない）。安定な並べ替えなので、同じ鍵は元の順のまま
            let mut keyed: Vec<((bool, f32, f32), &Entry)> =
                items.iter().map(|e| (hue_sort_key(e.color, hue_space), *e)).collect();
            keyed.sort_by(|(ka, _), (kb, _)| {
                ka.0.cmp(&kb.0)
                    .then(ka.1.total_cmp(&kb.1))
                    .then(ka.2.total_cmp(&kb.2))
            });
            items.clear();
            items.extend(keyed.into_iter().map(|(_, e)| e));
        }
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
        let (pins, rest) = l.view(SortMode::Recent, HueSpace::Hsv, "");
        assert_eq!((pins.len(), rest.len()), (1, 1));
        assert_eq!(rest[0].label, "空の青");
        assert_eq!(l.view(SortMode::Recent, HueSpace::Hsv, "#FF80").0.len(), 1);
        assert_eq!(l.view(SortMode::Recent, HueSpace::Hsv, "空").1.len(), 1);
        assert_eq!(l.view(SortMode::Recent, HueSpace::Hsv, "0,128,255").1.len(), 1);
    }

    #[test]
    fn view_search_ignores_width_case_and_kana() {
        let mut l = ColorList::new("x", "x", 0);
        l.record(rgb(0xff8000), "", 1, 500);
        l.record(rgb(0x0080ff), "", 2, 500);
        l.set_label(rgb(0x0080ff), "ブルー Sky");
        let n = |q: &str| l.view(SortMode::Recent, HueSpace::Hsv, q).1.len();
        assert_eq!(n("ＦＦ８０"), 1, "全角のカラーコード");
        assert_eq!(n("ぶるー"), 1, "ひらがなでカタカナのラベル");
        assert_eq!(n("ﾌﾞﾙｰ"), 1, "半角カナ");
        assert_eq!(n("ＳＫＹ"), 1, "全角の英字");
        assert_eq!(n("０，１２８，２５５"), 1, "全角の 255,128,0 形式");
        assert_eq!(n("zzz"), 0);
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
        assert_eq!(hexes(l.view(SortMode::Recent, HueSpace::Hsv, "").1), ["0000ff", "00ff00", "808080", "ff0000"]);
        assert_eq!(hexes(l.view(SortMode::Frequent, HueSpace::Hsv, "").1)[0], "0000ff");
        assert_eq!(hexes(l.view(SortMode::Hue, HueSpace::Hsv, "").1), ["ff0000", "00ff00", "0000ff", "808080"]);
    }

    /// 決まった種から色を作る（同じ色・灰色・暗い色も混ぜる）
    fn sample_list(n: usize) -> ColorList {
        let mut l = ColorList::new("x", "x", 0);
        let mut seed = 0x9e3779b9u32;
        for i in 0..n {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let c = match i % 7 {
                0 => {
                    let v = (seed & 0xff) as u8;
                    Rgb::new(v, v, v)
                }
                1 => rgb(seed & 0x1f1f1f),
                _ => rgb(seed & 0xffffff),
            };
            l.record(c, "", i as u64, MAX_ENTRIES_MAX);
        }
        l
    }

    /// v0.3.1 の色相順をそのまま写したもの（比較のたびに HSV を計算する）
    fn sort_hue_v031(items: &mut [&Entry]) {
        items.sort_by(|a, b| {
            let key = |e: &Entry| {
                let (h, s, v) = e.color.hsv();
                let achromatic = s < 0.12 || v < 0.08;
                (achromatic, if achromatic { -v } else { h }, -v)
            };
            let (ka, kb) = (key(a), key(b));
            ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1)).then(ka.2.total_cmp(&kb.2))
        });
    }

    /// 既定（HSV）の色相順は v0.3.1 と同じ並び（同じ鍵の色の順も含めて）
    #[test]
    fn default_hue_sort_matches_v031() {
        let l = sample_list(3000);
        assert!(l.entries.len() > 2500);
        let mut old: Vec<&Entry> = l.entries.iter().collect();
        sort_hue_v031(&mut old);
        let (pins, rest) = l.view(SortMode::Hue, HueSpace::default(), "");
        assert!(pins.is_empty());
        let colors = |v: &[&Entry]| v.iter().map(|e| e.color).collect::<Vec<_>>();
        assert_eq!(colors(&rest), colors(&old));
    }

    /// Oklch の色相順: 有彩色は Oklch の色相が増える順、無彩色（C・L が小さい）は後ろに明るい順
    #[test]
    fn oklch_hue_sort_follows_oklch_hue() {
        let l = sample_list(2000);
        let (_, rest) = l.view(SortMode::Hue, HueSpace::Oklch, "");
        assert_eq!(rest.len(), l.entries.len());
        let lch: Vec<[f64; 3]> = rest.iter().map(|e| crate::colorspace::oklch(e.color)).collect();
        let achromatic = |c: &[f64; 3]| c[1] < 0.025 || c[0] < 0.19;
        let first_gray = lch.iter().position(achromatic).expect("灰色がある");
        assert!(first_gray > 0, "有彩色がある");
        assert!(lch[first_gray..].iter().all(achromatic), "無彩色は後ろにまとまる");
        assert!(lch[..first_gray].windows(2).all(|w| w[0][2] <= w[1][2] + 1e-4), "有彩色は Oklch の色相の順");
        assert!(lch[first_gray..].windows(2).all(|w| w[0][0] >= w[1][0] - 1e-4), "無彩色は明るい順");
        // HSV の並びとは違う（青紫やピンクの位置が変わる）
        let (_, hsv) = l.view(SortMode::Hue, HueSpace::Hsv, "");
        assert_ne!(rest.iter().map(|e| e.color).collect::<Vec<_>>(), hsv.iter().map(|e| e.color).collect::<Vec<_>>());
    }

    /// v0.3.1 が書いた履歴ファイル（`tests/fixtures/history_v0.3.1.json`）は、足した設定を既定にして読む
    #[test]
    fn reads_v031_file_with_new_settings_at_default() {
        let text = include_str!("../tests/fixtures/history_v0.3.1.json");
        let mut s: Store = serde_json::from_str(text).expect("v0.3.1 の履歴を読める");
        s.normalize();
        assert_eq!(s.settings.hue_space, HueSpace::Hsv, "色相順は今までの HSV のまま");
        assert_eq!(s.settings.replace_metric, ColorMetric::PerChannel, "置換は今までの各成分の差のまま");
        // 既にあった設定・履歴はそのまま
        assert_eq!(s.settings.sort, SortMode::Hue);
        assert_eq!(s.settings.copy_format, CopyFormat::Hash);
        assert!(!s.settings.auto_record);
        assert_eq!((s.settings.max_entries, s.settings.swatch_size), (300, 32.0));
        assert_eq!(s.active, "list-1791015201");
        assert_eq!(s.lists.len(), 2);
        assert_eq!(s.lists[0].entries.len(), 2);
        // 書き出すと新しい欄が足される。読み直しても同じ
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["settings"]["hue_space"], "Hsv");
        assert_eq!(v["settings"]["replace_metric"], "PerChannel");
        let back: Store = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    /// 新しい設定の値を知らない値（さらに新しい版が足した色差など）で読んでも、その項目だけ既定にする
    #[test]
    fn unknown_color_settings_fall_back_per_field() {
        let newer = r#"{"auto_record":false,"hue_space":"Cam16","replace_metric":{"Jzazbz":2},"sort":"Hue"}"#;
        let s: Settings = serde_json::from_str(newer).unwrap();
        assert_eq!(s.hue_space, HueSpace::Hsv);
        assert_eq!(s.replace_metric, ColorMetric::PerChannel);
        assert_eq!(s.sort, SortMode::Hue);
        assert!(!s.auto_record);
        let known: Settings = serde_json::from_str(r#"{"hue_space":"Oklch","replace_metric":"Ciede2000"}"#).unwrap();
        assert_eq!((known.hue_space, known.replace_metric), (HueSpace::Oklch, ColorMetric::Ciede2000));
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

    /// 新規・知らないプロジェクトはデフォルトに記録し、選んだリストはプロジェクトごとに覚える（ユーザーの指示、2026-10-24）
    #[test]
    fn a_list_named_like_the_project_is_picked_and_remembered() {
        let mut s = Store::default();
        let short = s.create_list("マリンブルー", 1);
        let long = s.create_list("マリンブルー・マジック♡", 2);
        s.set_active(DEFAULT_LIST_ID);
        let p = Path::new(r"D:\Videos\動画編集\マリンブルー・マジック♡\マリンブルー・マジック♡_New.aup2");
        let o = s.open_project(Some(p));
        assert_eq!((o.changed, o.reason, s.active.as_str()), (true, OpenReason::NameMatched, long.as_str()), "いちばん長く合うリスト");
        assert_eq!(s.project_lists.get(&project_key(p)), Some(&long), "合ったら覚える");
        // 覚えた後に選び直したら、そちらが勝つ（デフォルトを選んでも）
        s.set_active(DEFAULT_LIST_ID);
        s.open_project(None);
        assert_eq!(s.open_project(Some(p)).reason, OpenReason::Remembered);
        assert_eq!(s.active, DEFAULT_LIST_ID);
        // フォルダ名でも合う。合わなければデフォルト
        let q = Path::new(r"D:\Videos\マリンブルー\本編.aup2");
        assert_eq!(s.open_project(Some(q)).reason, OpenReason::NameMatched);
        assert_eq!(s.active, short);
        let o = s.open_project(Some(Path::new(r"D:\Videos\別\別.aup2")));
        assert_eq!((o.reason, s.active.as_str()), (OpenReason::Unknown, DEFAULT_LIST_ID));
        assert_eq!(s.open_project(None).reason, OpenReason::Unsaved);
    }

    #[test]
    fn each_project_remembers_its_list_and_unknown_ones_use_default() {
        let a = Path::new(r"D:\Videos\作品A\作品A.aup2");
        let b = Path::new(r"D:\Videos\作品B\作品B.aup2");
        let mut s = Store::default();
        s.open_project(Some(a));
        let la = s.create_list("作品A の色", 1);
        assert_eq!(s.active, la);
        // 知らないプロジェクト → デフォルト
        assert!(s.open_project(Some(b)).changed);
        assert_eq!(s.active, DEFAULT_LIST_ID);
        // 新規（未保存）→ デフォルト。そこで選んでも、保存するまでは覚えない
        s.open_project(Some(a));
        assert_eq!(s.active, la, "作品A に戻ると、作品A で選んだリスト");
        s.open_project(None);
        assert_eq!(s.active, DEFAULT_LIST_ID);
        let ln = s.create_list("新規の色", 2);
        assert!(s.project_lists.values().all(|v| v != &ln), "未保存のプロジェクトでは覚えない");
        // 新規を保存したら、選んでいたリストをその名前で覚える
        let c = Path::new(r"D:\Videos\作品C\作品C.aup2");
        assert!(s.project_saved(c));
        s.open_project(Some(b));
        s.open_project(Some(Path::new(r"d:/videos/作品c/作品C.aup2")));
        assert_eq!(s.active, ln, "大文字と小文字・区切りが違っても同じプロジェクト");
        // デフォルトを選び直したら覚えを外す。リストを消したら、そのリストの覚えも外れる
        s.open_project(Some(a));
        s.set_active(DEFAULT_LIST_ID);
        assert_eq!(s.project_lists.get(&project_key(a)).map(|s| s.as_str()), Some(DEFAULT_LIST_ID), "デフォルトを選んだことも覚える");
        s.delete_list(&ln);
        assert!(s.project_lists.values().all(|v| v != &ln), "消したリストの覚えは外れる");
        s.open_project(Some(c));
        assert_eq!(s.active, DEFAULT_LIST_ID);
        // 書き出して読み直しても覚えは残る。前の版のファイル（覚えの欄が無い）も読める
        s.open_project(Some(a));
        s.set_active(&la);
        let text = serde_json::to_string(&s).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(back.project_lists.get(&project_key(a)), Some(&la));
        let old: Store = serde_json::from_str(r#"{"version":2,"lists":[],"active":"default"}"#).unwrap();
        assert!(old.project_lists.is_empty());
    }
}
