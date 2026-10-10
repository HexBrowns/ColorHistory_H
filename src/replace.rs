//! プロジェクト内の色の検索と一括置換
//!
//! - **しきい値**: RGB の各成分の差が N 以下なら同じ色とみなす（N=0 は完全一致）
//! - **表示中のシーン**は本体の API（`call_read_section`）で探し、置換もできる
//! - **他のシーン**は最後に保存した `.aup2` を読んで一覧に出すだけ（API で他シーンは編集できない）
//! - 置換は**ボタン操作のときだけ**、1 回の `call_edit_section` にまとめる。
//!   本体の Undo に 1 回で戻せる単位として登録される
//! - 置換の直前に値を読み直し、まだ条件に合う項目だけを書き換える（検索後に変わった値を上書きしない）

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use aviutl2::generic::{EffectItemType, ObjectHandle};

use crate::color::Rgb;
use crate::edit_ops::{self, RawItem};
use crate::tracker::ColorItemKey;

pub const TOLERANCE_MAX: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    /// 色の設定項目
    ColorItem,
    /// テキスト本文の `<#rrggbb>` 制御文字
    TextTag,
}

#[derive(Debug, Clone, Copy)]
pub struct SearchQuery {
    pub target: Rgb,
    pub tolerance: u8,
    pub include_text: bool,
    pub all_scenes: bool,
}

#[derive(Clone)]
pub struct Hit {
    pub scene_id: i32,
    pub scene_name: String,
    /// 表示中のシーンのオブジェクトだけ持つ（＝置換できる）
    pub object: Option<ObjectHandle>,
    pub layer: usize,
    pub frame_start: usize,
    pub frame_end: usize,
    pub key: ColorItemKey,
    pub kind: HitKind,
    /// 見つかった色（テキストでは複数ありうる）
    pub found: Vec<Rgb>,
}

impl Hit {
    pub fn editable(&self) -> bool {
        self.object.is_some()
    }

    /// 「テキスト / 文字色」「テキスト / テキスト の <#>」
    pub fn place(&self) -> String {
        match self.kind {
            HitKind::ColorItem => self.key.label(),
            HitKind::TextTag => format!("{} の <#>", self.key.label()),
        }
    }
}

#[derive(Default)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub notes: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ReplaceReport {
    pub replaced: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

pub fn within(a: Rgb, b: Rgb, tolerance: u8) -> bool {
    let d = |x: u8, y: u8| x.abs_diff(y) <= tolerance;
    d(a.r, b.r) && d(a.g, b.g) && d(a.b, b.b)
}

/// テキスト中の `<#rrggbb>` / `<#rrggbb,rrggbb>` のカラーコード部分（6 桁の範囲と色）。
pub fn text_tag_spans(text: &str) -> Vec<(Range<usize>, Rgb)> {
    let bytes = text.as_bytes();
    let hex6 = |at: usize| -> Option<Rgb> {
        let s = text.get(at..at + 6)?;
        Rgb::from_item_value(s)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'#' {
            let first = i + 2;
            if let Some(c1) = hex6(first) {
                let mut spans = vec![(first..first + 6, c1)];
                let mut end = first + 6;
                if bytes.get(end) == Some(&b',') {
                    if let Some(c2) = hex6(end + 1) {
                        spans.push((end + 1..end + 7, c2));
                        end += 7;
                    }
                }
                if bytes.get(end) == Some(&b'>') {
                    out.extend(spans);
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// しきい値内の `<#…>` の色を置き換える。`(新しい本文, 置き換えた色の数)`。
pub fn replace_text_tags(text: &str, target: Rgb, tolerance: u8, new: Rgb) -> (String, usize) {
    let spans: Vec<Range<usize>> = text_tag_spans(text)
        .into_iter()
        .filter(|(_, c)| within(*c, target, tolerance) && *c != new)
        .map(|(r, _)| r)
        .collect();
    if spans.is_empty() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for r in &spans {
        out.push_str(&text[last..r.start]);
        out.push_str(&new.hex());
        last = r.end;
    }
    out.push_str(&text[last..]);
    (out, spans.len())
}

/// 1 項目を判定する。一致した色の列（無ければ `None`）。
fn match_value(value: &str, kind: HitKind, q: &SearchQuery) -> Option<Vec<Rgb>> {
    match kind {
        HitKind::ColorItem => Rgb::from_item_value(value)
            .filter(|c| within(*c, q.target, q.tolerance))
            .map(|c| vec![c]),
        HitKind::TextTag => {
            let found: Vec<Rgb> = text_tag_spans(value)
                .into_iter()
                .map(|(_, c)| c)
                .filter(|c| within(*c, q.target, q.tolerance))
                .collect();
            (!found.is_empty()).then_some(found)
        }
    }
}

/// effect.name → (色項目名, テキスト項目名)
pub type ItemTypesFn<'a> = &'a dyn Fn(&str) -> (HashSet<String>, HashSet<String>);

fn live_item_types(effect_name: &str) -> (HashSet<String>, HashSet<String>) {
    (
        edit_ops::item_names_of(effect_name, EffectItemType::Color),
        edit_ops::item_names_of(effect_name, EffectItemType::Text),
    )
}

struct ObjectItems {
    layer: usize,
    frame_start: usize,
    frame_end: usize,
    object: Option<ObjectHandle>,
    items: Vec<RawItem>,
}

fn collect_hits(
    scene_id: i32,
    scene_name: &str,
    objects: Vec<ObjectItems>,
    q: &SearchQuery,
    types: ItemTypesFn,
) -> Vec<Hit> {
    let mut cache: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();
    let mut hits = Vec::new();
    for obj in objects {
        for item in obj.items {
            let (colors, texts) = cache
                .entry(item.effect_name.clone())
                .or_insert_with(|| types(&item.effect_name));
            let kind = if colors.contains(&item.item_name) {
                HitKind::ColorItem
            } else if q.include_text && texts.contains(&item.item_name) {
                HitKind::TextTag
            } else {
                continue;
            };
            if let Some(found) = match_value(&item.value, kind, q) {
                hits.push(Hit {
                    scene_id,
                    scene_name: scene_name.to_string(),
                    object: obj.object,
                    layer: obj.layer,
                    frame_start: obj.frame_start,
                    frame_end: obj.frame_end,
                    key: ColorItemKey {
                        effect_name: item.effect_name,
                        effect_index: item.effect_index,
                        item_name: item.item_name,
                    },
                    kind,
                    found,
                });
            }
        }
    }
    hits
}

/// 表示中のシーンを本体の API で探す。`(シーン ID, シーン名, 見つかった項目)`
fn search_current_scene(q: &SearchQuery) -> Result<(i32, String, Vec<Hit>), String> {
    if !edit_ops::EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません".into());
    }
    // ReadSection は編集情報を持たないので、読み取りセクションに入る前に取る
    // （セクション内から呼んだときのロックの再入を避ける）
    let info = edit_ops::EDIT_HANDLE.get_edit_info();
    let (scene_id, layer_max) = (info.scene_id, info.layer_max);
    let (scene_name, objects) = edit_ops::with_read_section(move |read| {
        let scene_name: String = read.get_scene_name().unwrap_or_default();
        let mut objects = Vec::new();
        for layer in 0..=layer_max {
            for (lf, handle) in read.objects_in_layer(layer) {
                let Ok(alias) = read.object(handle).get_alias_parsed() else {
                    continue;
                };
                objects.push(ObjectItems {
                    layer: lf.layer,
                    frame_start: lf.start,
                    frame_end: lf.end,
                    object: Some(handle),
                    items: edit_ops::collect_items(&alias),
                });
            }
        }
        Ok((scene_name, objects))
    })?;
    // 項目の種類の問い合わせは読み取りセクションの外で行う
    let hits = collect_hits(scene_id, &scene_name, objects, q, &live_item_types);
    Ok((scene_id, scene_name, hits))
}

/// `.aup2` を読んだ結果。
#[derive(Debug, Default)]
struct Aup2 {
    scenes: HashMap<i32, String>,
    objects: Vec<Aup2Object>,
}

#[derive(Debug, Default)]
struct Aup2Object {
    scene: i32,
    layer: usize,
    frame_start: usize,
    frame_end: usize,
    /// (effect.name, 項目) を効果の順に
    effects: Vec<(String, Vec<(String, String)>)>,
}

fn parse_aup2(text: &str) -> Aup2 {
    enum Section {
        Other,
        Scene(i32),
        Object(usize),
        Effect(usize),
    }
    let mut aup = Aup2::default();
    let mut index_of: HashMap<usize, usize> = HashMap::new();
    let mut section = Section::Other;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = if let Some(n) = name.strip_prefix("scene.") {
                n.parse().map(Section::Scene).unwrap_or(Section::Other)
            } else if let Some((obj, _eff)) = name.split_once('.') {
                match (obj.parse::<usize>(), _eff.parse::<usize>()) {
                    (Ok(o), Ok(_)) if index_of.contains_key(&o) => {
                        let idx = index_of[&o];
                        aup.objects[idx].effects.push((String::new(), Vec::new()));
                        Section::Effect(idx)
                    }
                    _ => Section::Other,
                }
            } else if let Ok(o) = name.parse::<usize>() {
                index_of.insert(o, aup.objects.len());
                aup.objects.push(Aup2Object::default());
                Section::Object(aup.objects.len() - 1)
            } else {
                Section::Other
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match section {
            Section::Scene(id) if key == "name" => {
                aup.scenes.insert(id, value.to_string());
            }
            Section::Object(idx) => {
                let obj = &mut aup.objects[idx];
                match key {
                    "layer" => obj.layer = value.parse().unwrap_or(0),
                    "scene" => obj.scene = value.parse().unwrap_or(0),
                    "frame" => {
                        let frames: Vec<usize> = value.split(',').filter_map(|f| f.trim().parse().ok()).collect();
                        obj.frame_start = frames.first().copied().unwrap_or(0);
                        obj.frame_end = frames.last().copied().unwrap_or(0);
                    }
                    _ => {}
                }
            }
            Section::Effect(idx) => {
                if let Some((name, items)) = aup.objects[idx].effects.last_mut() {
                    if key == "effect.name" {
                        *name = value.to_string();
                    } else {
                        items.push((key.to_string(), value.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    aup
}

/// 保存済みの `.aup2` から、`skip_scene` 以外のシーンを探す（置換はできない）。
fn search_saved_project(text: &str, skip_scene: i32, q: &SearchQuery, types: ItemTypesFn) -> Vec<Hit> {
    let aup = parse_aup2(text);
    let mut by_scene: HashMap<i32, Vec<ObjectItems>> = HashMap::new();
    for obj in aup.objects {
        if obj.scene == skip_scene {
            continue;
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut items = Vec::new();
        for (effect_name, effect_items) in obj.effects {
            let counter = seen.entry(effect_name.clone()).or_insert(0);
            let effect_index = *counter;
            *counter += 1;
            for (item_name, value) in effect_items {
                items.push(RawItem { effect_name: effect_name.clone(), effect_index, item_name, value });
            }
        }
        by_scene.entry(obj.scene).or_default().push(ObjectItems {
            layer: obj.layer,
            frame_start: obj.frame_start,
            frame_end: obj.frame_end,
            object: None,
            items,
        });
    }
    let mut scenes: Vec<i32> = by_scene.keys().copied().collect();
    scenes.sort();
    let mut hits = Vec::new();
    for id in scenes {
        let name = aup.scenes.get(&id).cloned().unwrap_or_else(|| format!("シーン {id}"));
        let objects = by_scene.remove(&id).unwrap_or_default();
        hits.extend(collect_hits(id, &name, objects, q, types));
    }
    hits
}

/// 検索する。表示中のシーン（置換できる）→ 他のシーン（保存データ）の順に並べる。
pub fn search(q: &SearchQuery, project_path: Option<&Path>) -> Result<SearchResult, String> {
    let (scene_id, _scene_name, mut hits) = search_current_scene(q)?;
    let mut result = SearchResult::default();
    if q.all_scenes {
        match project_path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => {
                    let others = search_saved_project(&text, scene_id, q, &live_item_types);
                    if !others.is_empty() {
                        result.notes.push("他のシーンは最後に保存した内容から探しています（置換できるのは表示中のシーンだけ）".into());
                    }
                    hits.extend(others);
                }
                Err(e) => result.notes.push(format!("保存済みのプロジェクトを読めませんでした: {e}")),
            },
            None => result.notes.push("プロジェクトが未保存なので、他のシーンは探していません".into()),
        }
    }
    hits.sort_by_key(|h| (!h.editable(), h.scene_id, h.layer, h.frame_start));
    result.hits = hits;
    Ok(result)
}

/// 置換する項目が、今表示しているシーンで見つけたものか。
/// 違うシーンのものが 1 つでもあれば理由を返す（検索の後にシーンを切り替えた。ハンドルは別のシーンのオブジェクトを指す）。
fn scene_mismatch(hits: &[Hit], current_scene: i32) -> Option<String> {
    let other = hits
        .iter()
        .filter(|h| h.editable())
        .find(|h| h.scene_id != current_scene)?;
    Some(format!(
        "検索した後にシーンが切り替わっています（検索したのは「{}」）。何も書き換えていません。もう一度検索してください",
        other.scene_name
    ))
}

/// チェックした項目を置き換える（ボタン操作専用）。1 回の編集セクション＝本体の Undo 1 回ぶん。
/// 検索した後にシーンが切り替わっていたら、1 つも書かずに `Err` を返す。
pub fn apply(hits: Vec<Hit>, target: Rgb, tolerance: u8, new: Rgb) -> Result<ReplaceReport, String> {
    edit_ops::with_edit_section(move |edit| {
        if let Some(reason) = scene_mismatch(&hits, edit.info.scene_id) {
            tracing::warn!(current_scene = edit.info.scene_id, "replace: scene changed after search");
            return Err(reason);
        }
        let mut report = ReplaceReport::default();
        for hit in hits {
            let Some(handle) = hit.object else {
                report.skipped += 1;
                continue;
            };
            let key = &hit.key;
            let object = edit.object(handle);
            let current = match object.get_effect_item(&key.effect_name, key.effect_index, &key.item_name) {
                Ok(v) => v,
                Err(e) => {
                    report.errors.push(format!("{}: 読めませんでした（{e:?}）", hit.place()));
                    continue;
                }
            };
            // 検索後に値が変わっていたら書き換えない
            let next = match hit.kind {
                HitKind::ColorItem => Rgb::from_item_value(&current)
                    .filter(|c| within(*c, target, tolerance) && *c != new)
                    .map(|_| new.hex()),
                HitKind::TextTag => {
                    let (text, n) = replace_text_tags(&current, target, tolerance, new);
                    (n > 0).then_some(text)
                }
            };
            let Some(next) = next else {
                report.skipped += 1;
                continue;
            };
            if let Err(e) = object.set_effect_item(&key.effect_name, key.effect_index, &key.item_name, &next) {
                tracing::warn!(place = %hit.place(), error = ?e, "replace: set failed");
                report.errors.push(format!("{}: 書き込めませんでした（{e:?}）", hit.place()));
                continue;
            }
            match object.get_effect_item(&key.effect_name, key.effect_index, &key.item_name) {
                Ok(written) if written.eq_ignore_ascii_case(&next) => report.replaced += 1,
                Ok(written) => {
                    tracing::warn!(place = %hit.place(), wrote = %next, read_back = %written, "replace: value did not stick");
                    report.errors.push(format!("{}: 反映されませんでした", hit.place()));
                }
                Err(_) => report.replaced += 1,
            }
        }
        Ok(report)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(n: u32) -> Rgb {
        Rgb::new((n >> 16) as u8, (n >> 8) as u8, n as u8)
    }

    fn q(target: u32, tolerance: u8) -> SearchQuery {
        SearchQuery { target: rgb(target), tolerance, include_text: true, all_scenes: true }
    }

    #[test]
    fn tolerance_is_per_channel() {
        assert!(within(rgb(0x808080), rgb(0x808080), 0));
        assert!(!within(rgb(0x818080), rgb(0x808080), 0));
        assert!(within(rgb(0x818080), rgb(0x808080), 1), "1 だけ違う色をまとめられる");
        assert!(within(rgb(0x7f817f), rgb(0x808080), 1));
        assert!(!within(rgb(0x828080), rgb(0x808080), 1));
    }

    #[test]
    fn text_tags_are_found() {
        let text = "赤<#ff0000>文字<#>と<#00ff00,000000>縁 <#zzzzzz> <#12345> <#abcdef";
        let spans = text_tag_spans(text);
        let colors: Vec<String> = spans.iter().map(|(_, c)| c.hex()).collect();
        assert_eq!(colors, ["ff0000", "00ff00", "000000"]);
        for (r, c) in &spans {
            assert_eq!(&text[r.clone()], c.hex());
        }
    }

    #[test]
    fn text_tags_are_replaced_within_tolerance() {
        let text = "<#ff0000>A<#fe0101,ff0000>B<#00ff00>";
        let (out, n) = replace_text_tags(text, rgb(0xff0000), 1, rgb(0x123456));
        assert_eq!(n, 3);
        assert_eq!(out, "<#123456>A<#123456,123456>B<#00ff00>");
        let (same, n) = replace_text_tags(text, rgb(0x0000ff), 0, rgb(0x123456));
        assert_eq!((same.as_str(), n), (text, 0));
    }

    const AUP2: &str = "[project]\r\nversion=2010900\r\n[scene.0]\r\nscene=0\r\nname=Root\r\n[0]\r\nlayer=0\r\nframe=0,59\r\n[0.0]\r\neffect.name=テキスト\r\n文字色=ff0000\r\nテキスト=<#ff0101>強調\r\n[0.1]\r\neffect.name=標準描画\r\nX=0.00\r\n[scene.1]\r\nscene=1\r\nname=サブ\r\n[1]\r\nlayer=2\r\nscene=1\r\nframe=10,20,30\r\n[1.0]\r\neffect.name=図形\r\n色=fe0000\r\n[1.1]\r\neffect.name=縁取り\r\n縁色=00ff00\r\n[1.2]\r\neffect.name=縁取り\r\n縁色=ff0000\r\n";

    fn fake_types(effect: &str) -> (HashSet<String>, HashSet<String>) {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        match effect {
            "テキスト" => (set(&["文字色", "影・縁色"]), set(&["テキスト"])),
            "図形" => (set(&["色"]), set(&[])),
            "縁取り" => (set(&["縁色"]), set(&[])),
            _ => (set(&[]), set(&[])),
        }
    }

    #[test]
    fn parses_aup2_scenes_objects_effects() {
        let aup = parse_aup2(AUP2);
        assert_eq!(aup.scenes.get(&1).map(String::as_str), Some("サブ"));
        assert_eq!(aup.objects.len(), 2);
        let sub = &aup.objects[1];
        assert_eq!((sub.scene, sub.layer, sub.frame_start, sub.frame_end), (1, 2, 10, 30));
        assert_eq!(sub.effects.len(), 3);
        assert_eq!(sub.effects[2].0, "縁取り");
    }

    #[test]
    fn saved_project_search_skips_current_scene_and_counts_effect_index() {
        let hits = search_saved_project(AUP2, 0, &q(0xff0000, 1), &fake_types);
        assert!(hits.iter().all(|h| h.scene_id == 1 && !h.editable()));
        let places: Vec<String> = hits.iter().map(|h| h.place()).collect();
        assert_eq!(places, ["図形 / 色", "縁取り:1 / 縁色"]);
        assert_eq!(hits[0].scene_name, "サブ");
    }

    fn hit(scene_id: i32, editable: bool) -> Hit {
        // 置換の前の判定だけを見るので、ハンドルは中身の無い値でよい（本体の API には渡さない）
        let handle: aviutl2::sys::plugin2::OBJECT_HANDLE = 0x10usize as *mut std::ffi::c_void;
        Hit {
            scene_id,
            scene_name: format!("シーン{scene_id}"),
            object: editable.then(|| ObjectHandle::from(handle)),
            layer: 0,
            frame_start: 0,
            frame_end: 10,
            key: ColorItemKey { effect_name: "図形".into(), effect_index: 0, item_name: "色".into() },
            kind: HitKind::ColorItem,
            found: vec![rgb(0xff0000)],
        }
    }

    #[test]
    fn replace_refuses_when_scene_changed_after_search() {
        // 検索したシーンのままなら書いてよい（他のシーンの一覧は置換の対象外なので数えない）
        assert!(scene_mismatch(&[hit(0, true), hit(1, false)], 0).is_none());
        // 検索の後に別のシーンへ切り替えたら、1 つも書かない
        let reason = scene_mismatch(&[hit(0, true), hit(0, true)], 2).expect("切り替えを見つける");
        assert!(reason.contains("シーン0"), "{reason}");
        assert!(scene_mismatch(&[], 3).is_none());
    }

    #[test]
    fn saved_project_search_includes_text_tags_when_asked() {
        let mut query = q(0xff0000, 1);
        let hits = search_saved_project(AUP2, 1, &query, &fake_types);
        let places: Vec<String> = hits.iter().map(|h| h.place()).collect();
        assert_eq!(places, ["テキスト / 文字色", "テキスト / テキスト の <#>"]);
        query.include_text = false;
        assert_eq!(search_saved_project(AUP2, 1, &query, &fake_types).len(), 1);
    }
}
