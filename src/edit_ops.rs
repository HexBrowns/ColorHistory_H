//! 本体とのやり取り
//!
//! - **読み取りは `call_read_section` だけ**（共有ロックを取るだけで本体の Undo に触れない）
//! - **書き込みは「適用」ボタンを押したときだけ** `call_edit_section` を使う。
//!   イベントやポーリングから `call_edit_section` を呼ぶと、本体が UI 操作中の Undo を捨てる
//!   （`.claude/rules/au2-rs-plugin.md`「`call_edit_section` は操作中の本体 Undo を捨てる」）

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use aviutl2::generic::{EditHandleError, EditSection, EffectItemType, GlobalEditHandle, ReadSection};
use parking_lot::RwLock;

use crate::color::Rgb;
use crate::tracker::{ColorItem, ColorItemKey, ObjectId};

pub static EDIT_HANDLE: GlobalEditHandle = GlobalEditHandle::new();

/// effect.name → 設定項目の名前と種類。取得に失敗したエフェクトは空で覚える（毎回問い合わせない）。
static ITEM_TYPES: LazyLock<RwLock<HashMap<String, HashMap<String, EffectItemType>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn edit_handle_error(err: EditHandleError) -> String {
    format!("編集 API エラー: {err:?}")
}

pub(crate) fn with_read_section<T, F>(callback: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&ReadSection) -> Result<T, String> + Send + 'static,
{
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません".into());
    }
    EDIT_HANDLE
        .call_read_section(callback)
        .map_err(edit_handle_error)
        .and_then(|r| r)
}

pub(crate) fn with_edit_section<T, F>(callback: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut EditSection) -> Result<T, String> + Send + 'static,
{
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません".into());
    }
    EDIT_HANDLE
        .call_edit_section(callback)
        .map_err(edit_handle_error)
        .and_then(|r| r)
}

/// 本体の呼び出しで panic しても FFI 越しに伝播させない。
pub fn catch_panic<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "不明な panic".into());
            tracing::error!(panic = %msg, "ColorHistory_H edit API panic");
            Err(format!("内部エラー(panic): {msg}"))
        }
    }
}

/// そのエフェクトの設定項目の名前と種類。`get_effect_items` はロックを取らないので、編集セクションの外で呼ぶ。
fn item_types(effect_name: &str) -> HashMap<String, EffectItemType> {
    if let Some(types) = ITEM_TYPES.read().get(effect_name) {
        return types.clone();
    }
    // スクリプトは "セクション@スクリプト" のまま渡す。駄目なら @ より前でも試す
    let template = effect_name.split('@').next().unwrap_or(effect_name);
    let mut types = HashMap::new();
    for candidate in [effect_name, template] {
        if let Ok(items) = EDIT_HANDLE.get_effect_items(candidate) {
            types = items.into_iter().map(|i| (i.name, i.item_type)).collect();
            break;
        }
    }
    ITEM_TYPES.write().insert(effect_name.to_string(), types.clone());
    types
}

/// そのエフェクトの、指定した種類の項目名。
pub(crate) fn item_names_of(effect_name: &str, kind: EffectItemType) -> HashSet<String> {
    item_types(effect_name)
        .into_iter()
        .filter(|(_, t)| *t == kind)
        .map(|(n, _)| n)
        .collect()
}

fn color_item_names(effect_name: &str) -> HashSet<String> {
    item_names_of(effect_name, EffectItemType::Color)
}

fn object_id(handle: &aviutl2::generic::ObjectHandle) -> ObjectId {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    handle.hash(&mut h);
    h.finish()
}

#[derive(Debug, Clone)]
pub(crate) struct RawItem {
    pub effect_name: String,
    pub effect_index: usize,
    pub item_name: String,
    pub value: String,
}

/// エイリアスから全エフェクトの設定項目を拾う。`effect_index` は同名エフェクトの何番目か。
pub(crate) fn collect_items(alias: &aviutl2::alias::Table) -> Vec<RawItem> {
    fn walk(
        table: &aviutl2::alias::Table,
        out: &mut Vec<RawItem>,
        seen: &mut HashMap<String, usize>,
    ) {
        if let Some(effect_name) = table.get_value("effect.name") {
            let counter = seen.entry(effect_name.clone()).or_insert(0);
            let effect_index = *counter;
            *counter += 1;
            for (item_name, value) in table.values() {
                if item_name == "effect.name" {
                    continue;
                }
                out.push(RawItem {
                    effect_name: effect_name.clone(),
                    effect_index,
                    item_name: item_name.clone(),
                    value: value.clone(),
                });
            }
            return;
        }
        for (_, sub) in table.subtables() {
            walk(sub, out, seen);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashMap::new();
    walk(alias, &mut out, &mut seen);
    out
}

/// 色項目の住所の控え（v0.2.2）。同じオブジェクトで効果の並び（効果 ID の列）が変わらない間は、
/// エイリアス全体を読まずに色の項目だけを読む。中間点の多いオブジェクトはエイリアスが長く、本体に作らせるのが重い
/// （`issues/20261010_full_alias_read_on_every_update.md`）
#[derive(Debug, Clone)]
pub struct ColorLayout {
    object: ObjectId,
    effects: Vec<i64>,
    keys: Vec<ColorItemKey>,
    /// 項目ごとの、分かっている今の値（透明色は `None`）。`000000` を読んだときに引き継ぐ
    known: Vec<Option<Rgb>>,
    /// 最後にエイリアス全体を読んだ時刻
    alias_at: Instant,
}

/// `000000` を読んだ項目があるとき、エイリアスで確かめ直す間隔（透明と黒を直接行き来したのを拾う）
const BLACK_RECHECK: Duration = Duration::from_secs(2);

/// 項目だけを読んだときの `000000`。**透明色の項目は `get_object_effect_item` では `000000` で返る**
/// （エイリアスでは空。実機 2026-10-10、v0.2.2 で透明のまま黒が履歴に入った）ので、黒か透明か分からない
fn is_black_raw(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("000000")
}

/// 項目だけを読んだ値を色にする。`000000` は、前に分かっている値が黒か透明ならそれを引き継ぐ。
/// それ以外（別の色から `000000` になった）は黒か透明か決められないので `None`（エイリアスで確かめる）
pub(crate) fn resolve_item_values(raws: &[String], known: &[Option<Rgb>]) -> Option<Vec<Option<Rgb>>> {
    let black = Rgb::from_item_value("000000");
    raws.iter()
        .zip(known)
        .map(|(raw, prev)| {
            if !is_black_raw(raw) {
                return Some(Rgb::from_item_value(raw));
            }
            (prev.is_none() || *prev == black).then_some(*prev)
        })
        .collect()
}

enum Read {
    NoObject,
    /// 控えのとおりに色の項目だけを読んだ（値は `resolve_item_values` を通した後）
    Items(ObjectId, Vec<(ColorItemKey, Option<Rgb>)>),
    /// エイリアス全体を読んだ（効果の並びが取れなければ `None`）
    Alias(ObjectId, Option<Vec<i64>>, Vec<RawItem>),
}

/// フォーカス中オブジェクトの色項目。未選択なら `None`。`layout` は前回の控えで、読み直したら差し替える
pub fn read_focused_colors(layout: &mut Option<ColorLayout>) -> Result<Option<(ObjectId, Vec<ColorItem>)>, String> {
    let cached = layout.clone();
    let got = with_read_section(move |read| {
        let Some(object) = read.get_focused_object().map_err(|e| format!("{e:?}"))? else {
            return Ok(Read::NoObject);
        };
        let id = object_id(&object);
        let effects: Option<Vec<i64>> = read
            .get_effects(object)
            .ok()
            .map(|v| v.into_iter().map(|e| read.get_effect_id(e).unwrap_or(-1)).collect());
        if let (Some(c), Some(e)) = (&cached, &effects) {
            if c.object == id && c.effects == *e {
                let raws: Option<Vec<String>> = c
                    .keys
                    .iter()
                    .map(|k| read.get_object_effect_item(object, &k.effect_name, k.effect_index, &k.item_name).ok())
                    .collect();
                // 読めない項目があれば（名前が変わったなど）エイリアスから取り直す。
                // `000000` があるときは、黒か透明か決められないときと、前にエイリアスを読んでから時間がたったときも取り直す
                if let Some(raws) = raws {
                    let recheck = raws.iter().any(|r| is_black_raw(r)) && c.alias_at.elapsed() >= BLACK_RECHECK;
                    if let Some(values) = resolve_item_values(&raws, &c.known).filter(|_| !recheck) {
                        return Ok(Read::Items(id, c.keys.iter().cloned().zip(values).collect()));
                    }
                }
            }
        }
        let alias = read
            .object(object)
            .get_alias_parsed()
            .map_err(|e| format!("{e:?}"))?;
        Ok(Read::Alias(id, effects, collect_items(&alias)))
    })?;
    let (id, effects, raw) = match got {
        Read::NoObject => {
            *layout = None;
            return Ok(None);
        }
        Read::Items(id, values) => {
            if let Some(l) = layout.as_mut() {
                l.known = values.iter().map(|(_, v)| *v).collect();
            }
            let items = values.into_iter().map(|(key, value)| ColorItem { key, value }).collect();
            return Ok(Some((id, items)));
        }
        Read::Alias(id, effects, raw) => (id, effects, raw),
    };
    let mut names_by_effect: HashMap<String, HashSet<String>> = HashMap::new();
    let mut items = Vec::new();
    for r in raw {
        let names = names_by_effect
            .entry(r.effect_name.clone())
            .or_insert_with(|| color_item_names(&r.effect_name));
        if !names.contains(&r.item_name) {
            continue;
        }
        items.push(ColorItem {
            key: ColorItemKey {
                effect_name: r.effect_name,
                effect_index: r.effect_index,
                item_name: r.item_name,
            },
            value: Rgb::from_item_value(&r.value),
        });
    }
    // 効果の並びが取れたときだけ控える（取れなければ次も全体を読む）
    *layout = effects.map(|effects| ColorLayout {
        object: id,
        effects,
        keys: items.iter().map(|i| i.key.clone()).collect(),
        known: items.iter().map(|i| i.value).collect(),
        alias_at: Instant::now(),
    });
    Ok(Some((id, items)))
}

/// 「適用」ボタン専用。フォーカス中オブジェクトの色項目へ書き込み、読み返して確かめる。
pub fn apply_color(key: &ColorItemKey, color: Rgb) -> Result<(), String> {
    let key = key.clone();
    let value = color.hex();
    with_edit_section(move |edit| {
        let Some(handle) = edit.get_focused_object().map_err(|e| format!("{e:?}"))? else {
            return Err("選択中のオブジェクトがありません".into());
        };
        let object = edit.object(handle);
        // 書く前に、狙った項目が読めることを確かめる（範囲外の番号でも失敗が返らないことがある）
        object
            .get_effect_item(&key.effect_name, key.effect_index, &key.item_name)
            .map_err(|e| format!("適用先の項目が見つかりません（{}）: {e:?}", key.label()))?;
        object
            .set_effect_item(&key.effect_name, key.effect_index, &key.item_name, &value)
            .map_err(|e| {
                tracing::warn!(item = %key.label(), value = %value, error = ?e, "apply_color: set failed");
                format!("書き込めませんでした（{}）: {e:?}", key.label())
            })?;
        match object.get_effect_item(&key.effect_name, key.effect_index, &key.item_name) {
            Ok(written) if written.eq_ignore_ascii_case(&value) => Ok(()),
            Ok(written) => {
                tracing::warn!(item = %key.label(), wrote = %value, read_back = %written, "apply_color: value did not stick");
                Err(format!("書き込みが反映されませんでした（{}）。現在値: {written}", key.label()))
            }
            Err(e) => {
                tracing::warn!(item = %key.label(), error = ?e, "apply_color: read back failed");
                Ok(())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn black_is_ambiguous_with_transparent() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let black = Rgb::from_item_value("000000");
        let red = Rgb::from_item_value("ff0000");
        // 黒でない値はそのまま
        assert_eq!(resolve_item_values(&s(&["ff0000"]), &[None]), Some(vec![red]));
        // 透明だった項目が 000000 で返る → 透明のまま（v0.2.2 の最初の版はここで黒を記録した）
        assert_eq!(resolve_item_values(&s(&["000000"]), &[None]), Some(vec![None]));
        // 黒だった項目は黒のまま
        assert_eq!(resolve_item_values(&s(&["000000"]), &[black]), Some(vec![black]));
        // 別の色から 000000 になった → 黒か透明か決められないので、エイリアスで確かめる
        assert_eq!(resolve_item_values(&s(&["ff0000", "000000"]), &[red, red]), None);
    }

    const ALIAS: &str = "[Object]
frame=0,59
[Object.0]
effect.name=テキスト
文字色=ffffff
影・縁色=000000
テキスト=abc
[Object.1]
effect.name=縁取り
縁色=ff0000
[Object.2]
effect.name=縁取り
縁色=
[Object.3]
effect.name=標準描画
X=0.00
";

    #[test]
    fn collect_items_counts_same_named_effects() {
        let table = aviutl2::alias::Table::from_str(ALIAS).unwrap();
        let items = collect_items(&table);
        let edges: Vec<_> = items.iter().filter(|i| i.item_name == "縁色").collect();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].effect_index, 0);
        assert_eq!(edges[0].value, "ff0000");
        assert_eq!(edges[1].effect_index, 1);
        assert_eq!(edges[1].value, "");
        assert!(items.iter().all(|i| i.item_name != "effect.name"));
    }
}
