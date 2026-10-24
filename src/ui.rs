//! 色履歴ウィンドウ（egui）

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use aviutl2_eframe::{eframe, egui, AviUtl2EframeHandle};

use crate::color::{CopyFormat, Rgb};
use crate::colorspace::{ColorMetric, HueSpace};
use crate::history::{
    now_secs, Entry, SortMode, DEFAULT_LIST_ID, MAX_ENTRIES_DEFAULT, MAX_ENTRIES_MAX, MAX_ENTRIES_MIN,
};
use crate::egui_input::{self, NumberSpec};
use crate::replace::{self, Hit, SearchQuery, TOLERANCE_MAX};
use crate::tracker::{ColorItem, ColorItemKey};
use crate::ui_theme::{self, StatusKind};
use crate::watcher::Msg;
use crate::{clipboard, colors, edit_ops, eyedropper, SharedState};

/// 見本は押して使うので、当たり判定を `ui_theme::MIN_HIT` 以上にする
const SWATCH_MIN: f32 = ui_theme::MIN_HIT;
const SWATCH_MAX: f32 = 64.0;
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

enum Action {
    Select(Rgb),
    Copy(Rgb, CopyFormat),
    SetPinned(Rgb, bool),
    StartLabel(Rgb),
    Delete(Rgb),
    Apply(Rgb, ColorItemKey),
    CopyToList(Rgb, String),
}

/// 一括置換パネルの状態。
struct ReplaceState {
    find: String,
    to: String,
    tolerance: u8,
    all_scenes: bool,
    include_text: bool,
    /// 検索した条件（置換はこの条件で読み直して行う）
    query: Option<SearchQuery>,
    hits: Vec<Hit>,
    checked: Vec<bool>,
    notes: Vec<String>,
}

impl Default for ReplaceState {
    fn default() -> Self {
        Self {
            find: String::new(),
            to: String::new(),
            tolerance: 0,
            all_scenes: true,
            include_text: true,
            query: None,
            hits: Vec::new(),
            checked: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// 並べ替えた表示（履歴・設定・検索語が変わったときだけ作り直す。ルール au2-rs-plugin「ウィンドウの重さ」）
struct ViewCache {
    revision: u64,
    search: String,
    pins: Vec<Entry>,
    rest: Vec<Entry>,
    other_lists: Vec<(String, String)>,
    list_name: String,
    size: f32,
    copy_format: CopyFormat,
}

pub struct ColorHistoryApp {
    _handle: AviUtl2EframeHandle,
    /// 本体のフォント（窓が見えてから読む。`host_fonts`）
    host_fonts: crate::host_fonts::LazyFonts,
    shared: SharedState,
    watcher: Option<Sender<Msg>>,
    search: String,
    /// 絞り込みに使う検索語（IME の変換中は変換を始める前のまま）
    search_query: String,
    selected: Option<Rgb>,
    apply_target: Option<ColorItemKey>,
    label_edit: Option<(Rgb, String)>,
    /// ラベルの編集を始めたフレームで、入力欄にフォーカスを移す
    label_focus: bool,
    add_input: String,
    show_settings: bool,
    clear_armed_at: Option<Instant>,
    lists_open: bool,
    list_rename: Option<(String, String)>,
    list_delete_armed: Option<(String, Instant)>,
    picking: bool,
    pick_preview: Option<Rgb>,
    replace_open: bool,
    replace: ReplaceState,
    view_cache: Option<ViewCache>,
    /// 窓が最後に描いた記録先の ID（変わったらログに残す）
    shown_active: String,
}

fn elapsed_text(then: u64, now: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=59 => "たった今".into(),
        60..=3599 => format!("{} 分前", d / 60),
        3600..=86_399 => format!("{} 時間前", d / 3600),
        _ => format!("{} 日前", d / 86_400),
    }
}

/// 件数の上限の欄（確定したときだけ反映する。範囲・速さは従来どおり）
fn max_entries_spec() -> NumberSpec {
    NumberSpec::new(MAX_ENTRIES_MIN as f64..=MAX_ENTRIES_MAX as f64, 2.0).default_value(MAX_ENTRIES_DEFAULT as f64)
}

/// 一括置換のしきい値の欄
fn tolerance_spec() -> NumberSpec {
    NumberSpec::new(0.0..=TOLERANCE_MAX as f64, 1.0).default_value(0.0)
}

/// 全リストの色の合計（上限を下げて消えた件数を数えるため）
fn total_entries(store: &crate::history::Store) -> usize {
    store.lists.iter().map(|l| l.entries.len()).sum()
}

/// 消去のボタンの文字。何件を・どのリストから消すかを出し、1 回目は「もう一度押す」を添える
fn clear_button_label(list_name: &str, count: usize, armed: bool) -> String {
    if armed {
        format!("もう一度押すと、「{list_name}」のピン留め以外 {count} 件を消去")
    } else {
        format!("「{list_name}」のピン留め以外 {count} 件を消去")
    }
}

/// 1 行の編集欄（ラベル・リスト名）。Enter かほかをクリックで `Some(true)`（確定）、Esc で `Some(false)`（やめる）。
/// `focus` が真のあいだ、フォーカスが無ければ欄に移す
fn edit_line(ui: &mut egui::Ui, text: &mut String, width: f32, focus: bool) -> Option<bool> {
    let out = egui_input::text_field(ui, text, |t| t.desired_width(width));
    if focus && !out.response.has_focus() && !out.response.lost_focus() {
        out.response.request_focus();
    }
    if !out.response.lost_focus() {
        return None;
    }
    // 変換中にほかをクリックしたときも、欄を残さない（変換中の文字は捨てられる）
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    Some(!escape)
}

fn small_swatch(ui: &mut egui::Ui, c: Option<Rgb>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    match c {
        Some(c) => {
            ui.painter().rect_filled(rect, 2.0, colors::from_data(c));
        }
        None => {
            ui.painter()
                .rect_stroke(rect, 2.0, egui::Stroke::new(1.0, colors::SWATCH_EMPTY_STROKE), egui::StrokeKind::Inside);
        }
    }
}

impl ColorHistoryApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        handle: AviUtl2EframeHandle,
        shared: SharedState,
        watcher: Option<Sender<Msg>>,
    ) -> Self {
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals = aviutl2_eframe::aviutl2_visuals();
        });
        // 本体のフォントは窓が見えてから ui で読む（ここで読むと、本体の起動がこの窓の初期化を待つ間ずっと止まる。`host_fonts`）
        let host_fonts = crate::host_fonts::LazyFonts::new(&handle);
        shared.write().egui_ctx = Some(cc.egui_ctx.clone());
        Self {
            _handle: handle,
            host_fonts,
            shared,
            watcher,
            search: String::new(),
            search_query: String::new(),
            selected: None,
            apply_target: None,
            label_edit: None,
            label_focus: false,
            add_input: String::new(),
            show_settings: false,
            clear_armed_at: None,
            lists_open: true,
            list_rename: None,
            list_delete_armed: None,
            picking: false,
            pick_preview: None,
            replace_open: false,
            replace: ReplaceState::default(),
            view_cache: None,
            shown_active: String::new(),
        }
    }

    fn set_status(&self, msg: impl Into<String>) {
        self.say(StatusKind::Info, msg);
    }

    /// 状態の行に、種類（色）つきで出す
    fn say(&self, kind: StatusKind, msg: impl Into<String>) {
        self.shared.write().say(kind, msg);
    }

    fn copy(&self, color: Rgb, format: CopyFormat) {
        let text = format.format(color);
        match clipboard::set_text(&text) {
            Ok(()) => self.say(StatusKind::Ok, format!("コピーしました: {text}")),
            Err(e) => self.say(
                StatusKind::Error,
                format!("コピーできませんでした: {e}。ほかのアプリがクリップボードを使っていないか確かめて、もう一度試してください"),
            ),
        }
    }

    fn apply(&mut self, color: Rgb, key: &ColorItemKey) {
        // ボタン操作を起点にしたときだけ書き込む（イベントからは書かない）
        match edit_ops::catch_panic(|| edit_ops::apply_color(key, color)) {
            Ok(()) => self.say(StatusKind::Ok, format!("{} に {} を適用しました", key.label(), color.hex())),
            Err(e) => self.say(StatusKind::Error, format!("適用できませんでした: {e}")),
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Select(c) => self.selected = Some(c),
                Action::Copy(c, f) => {
                    self.selected = Some(c);
                    self.copy(c, f);
                }
                Action::SetPinned(c, pinned) => {
                    let mut s = self.shared.write();
                    let max = s.store.settings.max_entries;
                    if s.store.active_list_mut().set_pinned(c, pinned, max) {
                        s.mark_dirty();
                    }
                }
                Action::StartLabel(c) => {
                    let current = self
                        .shared
                        .read()
                        .store
                        .active_list()
                        .get(c)
                        .map(|e| e.label.clone())
                        .unwrap_or_default();
                    self.label_edit = Some((c, current));
                    self.label_focus = true;
                }
                Action::Delete(c) => {
                    let mut s = self.shared.write();
                    if s.store.active_list_mut().remove(c).is_some() {
                        s.mark_dirty();
                        s.say(StatusKind::Ok, format!("{} をこのリストから消しました", c.hex()));
                    }
                    if self.selected == Some(c) {
                        self.selected = None;
                    }
                }
                Action::Apply(c, key) => self.apply(c, &key),
                Action::CopyToList(c, to) => {
                    let mut s = self.shared.write();
                    let from = s.store.active.clone();
                    if s.store.copy_entry_to(&from, c, &to) {
                        let name = s.store.lists.iter().find(|l| l.id == to).map(|l| l.name.clone()).unwrap_or_default();
                        s.mark_dirty();
                        s.say(StatusKind::Ok, format!("{} を「{name}」へコピーしました", c.hex()));
                    }
                }
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        // 処理したキーは消費する（aviutl2-eframe が本体へ転送しないように）
        let copy = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C));
        if copy {
            if let Some(c) = self.selected {
                let format = self.shared.read().store.settings.copy_format;
                self.copy(c, format);
            }
        }
    }

    /// スポイト: ボタンを押したままドラッグしている間は色を見せ、離した位置の色を記録する。
    /// winit がボタン押下時に `SetCapture` するので、ウィンドウの外で離してもボタンを離す通知は本体へ行かない
    /// （winit 0.30 `capture_mouse`）。押下状態は `GetAsyncKeyState` で見るので egui のイベントに頼らない。
    fn update_picking(&mut self, ctx: &egui::Context) {
        if !self.picking {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.picking = false;
            self.pick_preview = None;
            self.set_status("スポイトを中止しました");
            return;
        }
        if eyedropper::primary_button_down() {
            self.pick_preview = eyedropper::color_at_cursor().map(|(c, _)| c);
            ctx.request_repaint();
            return;
        }
        self.picking = false;
        self.pick_preview = None;
        match eyedropper::color_at_cursor() {
            Some((c, _)) => {
                let mut s = self.shared.write();
                s.store.record(c, "スポイト", now_secs());
                s.mark_dirty();
                s.say(StatusKind::Ok, format!("画面の色 {} を記録しました", c.hex()));
                drop(s);
                self.selected = Some(c);
            }
            None => self.say(StatusKind::Error, "画面の色を読めませんでした。画面に表示されている場所で離して、もう一度試してください"),
        }
    }

    fn render_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.lists_open, "リスト");
            // 打つたびに絞り込む（軽い）。入力中の Esc で入力前の文字に戻し、IME の変換中は絞らない
            let search_width = ui.available_width().min(150.0);
            let search = egui_input::search_field(ui, &mut self.search, |t| {
                t.hint_text("検索: カラーコード・ラベル").desired_width(search_width)
            });
            self.search_query = search.query;
            let mut s = self.shared.write();
            let mut sort = s.store.settings.sort;
            let r = egui::ComboBox::from_id_salt("sort")
                .selected_text(sort.label())
                .show_ui(ui, |ui| {
                    for m in SortMode::ALL {
                        ui.selectable_value(&mut sort, m, m.label());
                    }
                })
                .response;
            crate::egui_input::wheel_select(ui, &r, &mut sort, &SortMode::ALL);
            if sort != s.store.settings.sort {
                s.store.settings.sort = sort;
                s.mark_dirty();
            }
            if sort == SortMode::Hue {
                let mut space = s.store.settings.hue_space;
                let r = egui::ComboBox::from_id_salt("hue_space")
                    .selected_text(space.label())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for h in HueSpace::ALL {
                            ui.selectable_value(&mut space, h, h.label());
                        }
                    })
                    .response;
                crate::egui_input::wheel_select(ui, &r, &mut space, &HueSpace::ALL);
                r
                    .on_hover_text("色相を測る色空間\nHSV: 今までの並び\nOklch: 見た目の色相の順（青と紫の間などが詰まらない）\nどちらも、彩度の低い色と暗い色は後ろに明るい順でまとめる");
                if space != s.store.settings.hue_space {
                    s.store.settings.hue_space = space;
                    s.mark_dirty();
                }
            }
            if ui
                .checkbox(&mut s.store.settings.auto_record, "自動記録")
                .on_hover_text("オブジェクトの色を変えたとき、確定した色を選択中のリストへ記録する（選んだだけの色は記録しない）")
                .changed()
            {
                s.mark_dirty();
            }
            drop(s);
            if ui
                .button("記録")
                .on_hover_text("選択中オブジェクトの色を今すぐ記録する\n編集メニュー「色履歴: 選択中オブジェクトの色を記録」にショートカットを割り当てられる")
                .clicked()
            {
                match &self.watcher {
                    Some(tx) => {
                        let _ = tx.send(Msg::RecordNow);
                    }
                    None => self.say(StatusKind::Error, "監視スレッドが動いていません。AviUtl2 を再起動してください"),
                }
            }
            let pick = ui
                .add(egui::Button::new(if self.picking { "スポイト中" } else { "スポイト" }).selected(self.picking))
                .on_hover_text("押したまま拾いたい場所までドラッグして離すと、その位置の画面の色を記録する（Esc で中止）\n編集メニュー「色履歴: カーソル位置の色を記録」にショートカットを割り当てることもできる\n拾うのは画面に表示されている色で、設定値と少しずれることがある")
                .interact(egui::Sense::drag());
            if pick.drag_started() {
                self.picking = true;
                ui.ctx().request_repaint();
            } else if pick.clicked() && !self.picking {
                self.set_status("スポイトは、ボタンを押したまま拾いたい場所までドラッグして離します");
            }
            ui.toggle_value(&mut self.replace_open, "置換")
                .on_hover_text("プロジェクト内でその色を使っている箇所を探して、まとめて置き換える");
            ui.toggle_value(&mut self.show_settings, "設定");
        });

        if self.picking {
            ui.horizontal(|ui| {
                small_swatch(ui, self.pick_preview);
                let hex = self.pick_preview.map(|c| c.hex()).unwrap_or_else(|| "------".into());
                ui.label(format!("{hex}　離すと記録（Esc で中止）"));
            });
        }

        if self.show_settings {
            ui.separator();
            self.render_settings(ui);
        }
    }

    fn render_settings(&mut self, ui: &mut egui::Ui) {
        let mut s = self.shared.write();
        ui.horizontal_wrapped(|ui| {
            ui.label("件数の上限");
            let mut max = s.store.settings.max_entries;
            let out = egui_input::number_field(ui, &mut max, &max_entries_spec());
            out.response.on_hover_text(
                "1 つのリストでピン留めしていない色の最大件数。超えたら最後に使ったのが古い色から消える\n右クリックで既定値（500）に戻す",
            );
            // 上限を下げると古い色が消えるので、確定したとき（Enter・ほかをクリック・ドラッグを離した）だけ反映し、消した件数を知らせる
            if out.committed && max != s.store.settings.max_entries {
                let before = total_entries(&s.store);
                s.store.settings.max_entries = max;
                s.store.apply_limit();
                let removed = before - total_entries(&s.store);
                s.mark_dirty();
                if removed > 0 {
                    s.say(
                        StatusKind::Warn,
                        format!("件数の上限を {max} にしたので、最後に使ったのが古い色を {removed} 件消しました（ピン留めは残しています）"),
                    );
                }
            }
            ui.label("見本の大きさ");
            if egui_input::slider_field(ui, &mut s.store.settings.swatch_size, SWATCH_MIN..=SWATCH_MAX, |sl| sl).changed() {
                s.mark_dirty();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("クリックでコピーする形式");
            let mut format = s.store.settings.copy_format;
            let r = egui::ComboBox::from_id_salt("copy_format")
                .selected_text(format.label())
                .show_ui(ui, |ui| {
                    for f in CopyFormat::ALL {
                        ui.selectable_value(&mut format, f, f.label());
                    }
                })
                .response;
            crate::egui_input::wheel_select(ui, &r, &mut format, &CopyFormat::ALL);
            if format != s.store.settings.copy_format {
                s.store.settings.copy_format = format;
                s.mark_dirty();
            }
        });
        drop(s);
        ui.horizontal_wrapped(|ui| {
            ui.label("色を追加");
            let field = egui_input::text_field(ui, &mut self.add_input, |t| {
                t.hint_text("ff8000 / #ff8000 / 255,128,0").desired_width(140.0)
            });
            // 追加するのは Enter かボタンのときだけ（ほかをクリックしただけでは追加しない）。Esc は入力前の文字に戻す
            if ui.button("追加").clicked() || field.submitted {
                match Rgb::parse(&self.add_input) {
                    Some(c) => {
                        let mut s = self.shared.write();
                        s.store.record(c, "手入力", now_secs());
                        s.mark_dirty();
                        s.say(StatusKind::Ok, format!("{} を追加しました", c.hex()));
                        drop(s);
                        self.selected = Some(c);
                        self.add_input.clear();
                    }
                    None => self.say(
                        StatusKind::Error,
                        format!(
                            "「{}」をカラーコードとして読めません。ff8000・#ff8000・255,128,0 のどれかの形で入れてください",
                            self.add_input.trim()
                        ),
                    ),
                }
            }
        });
        // 壊す操作（消去）は、追加のボタンから離して別の行に置く。2 回押して初めて消す（件数と消す先を見せる）
        ui.add_space(ui_theme::GAP_SECTION);
        ui.horizontal(|ui| {
            let (n, list_name) = {
                let s = self.shared.read();
                let l = s.store.active_list();
                (l.entries.iter().filter(|e| !e.pinned).count(), l.name.clone())
            };
            let armed = self.clear_armed_at.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
            let label = clear_button_label(&list_name, n, armed);
            if ui.add_enabled_ui(n > 0, |ui| ui_theme::danger_button(ui, label)).inner.clicked() {
                if armed {
                    let mut s = self.shared.write();
                    let n = s.store.active_list_mut().clear_unpinned();
                    s.mark_dirty();
                    s.say(StatusKind::Ok, format!("{n} 件消去しました（ピン留めは残しています）"));
                    self.clear_armed_at = None;
                } else {
                    self.clear_armed_at = Some(Instant::now());
                }
            }
        });
    }

    fn render_lists(&mut self, ui: &mut egui::Ui) {
        ui.strong("リスト");
        ui.small("選んだリストに色が記録されます");
        ui.separator();
        let (lists, active) = {
            let s = self.shared.read();
            let lists: Vec<(String, String, usize)> =
                s.store.lists.iter().map(|l| (l.id.clone(), l.name.clone(), l.entries.len())).collect();
            (lists, s.store.active.clone())
        };
        egui::ScrollArea::vertical().id_salt("lists_scroll").show(ui, |ui| {
            for (id, name, count) in &lists {
                if let Some((rename_id, text)) = &mut self.list_rename {
                    if rename_id == id {
                        let rename_id = rename_id.clone();
                        let width = ui.available_width();
                        // Enter か欄の外をクリックで確定、Esc で取り消し（変換中は確定しない）
                        match edit_line(ui, text, width, true) {
                            Some(true) => {
                                let new_name = text.clone();
                                let mut s = self.shared.write();
                                if s.store.rename_list(&rename_id, &new_name) {
                                    s.mark_dirty();
                                }
                                drop(s);
                                self.list_rename = None;
                            }
                            Some(false) => self.list_rename = None,
                            None => {}
                        }
                        continue;
                    }
                }
                let response = ui
                    .selectable_label(&active == id, format!("{name}（{count}）"))
                    .on_hover_text("クリックで選択。以後の色はこのリストに記録される\n右クリック: 名前の変更・削除");
                if response.clicked() {
                    let mut s = self.shared.write();
                    if s.store.set_active(id) {
                        s.mark_dirty();
                        s.say(StatusKind::Ok, format!("記録先を「{name}」にしました"));
                    }
                    self.selected = None;
                }
                let is_default = id == DEFAULT_LIST_ID;
                response.context_menu(|ui| {
                    if ui.add_enabled(!is_default, egui::Button::new("名前を変更")).clicked() {
                        self.list_rename = Some((id.clone(), name.clone()));
                        ui.close();
                    }
                    let armed = self
                        .list_delete_armed
                        .as_ref()
                        .is_some_and(|(armed_id, t)| armed_id == id && t.elapsed() < CONFIRM_WINDOW);
                    let label = if armed {
                        format!("もう一度押すと、「{name}」（{count} 件）を削除")
                    } else {
                        "リストを削除".to_string()
                    };
                    // 壊す操作は、名前の変更から離して赤みのボタンにする
                    ui.add_space(ui_theme::GAP_SECTION);
                    if ui.add_enabled_ui(!is_default, |ui| ui_theme::danger_button(ui, label)).inner.clicked() {
                        if armed {
                            let mut s = self.shared.write();
                            if s.store.delete_list(id).is_some() {
                                s.mark_dirty();
                                s.say(StatusKind::Ok, format!("リスト「{name}」を削除しました"));
                            }
                            self.list_delete_armed = None;
                            ui.close();
                        } else {
                            self.list_delete_armed = Some((id.clone(), Instant::now()));
                        }
                    }
                    if is_default {
                        ui.small("デフォルトは名前の変更・削除ができません");
                    }
                });
            }
        });
        ui.separator();
        if ui.button("＋ 新規リスト").clicked() {
            let mut s = self.shared.write();
            let id = s.store.create_list("新しいリスト", now_secs());
            let name = s.store.lists.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or_default();
            s.mark_dirty();
            s.say(StatusKind::Ok, format!("リスト「{name}」を作り、記録先にしました"));
            drop(s);
            self.list_rename = Some((id, name));
            self.selected = None;
        }
    }

    fn render_bottom(&mut self, ui: &mut egui::Ui, focused: &Option<Vec<ColorItem>>) {
        if let Some((color, text)) = &mut self.label_edit {
            let color = *color;
            let mut done: Option<bool> = None;
            let focus = std::mem::take(&mut self.label_focus);
            ui.horizontal(|ui| {
                ui.label(format!("{} のラベル", color.hex()));
                // Enter かほかをクリックで保存、Esc でやめる（ラベルは編集前のまま）
                done = edit_line(ui, text, 160.0, focus);
                ui.small("Enter か欄の外をクリックで保存・Esc でやめる");
            });
            if let Some(save) = done {
                if save {
                    let mut s = self.shared.write();
                    if s.store.active_list_mut().set_label(color, text) {
                        s.mark_dirty();
                    }
                }
                self.label_edit = None;
            }
            ui.separator();
        }

        ui.horizontal_wrapped(|ui| {
            ui.label("適用先");
            match focused {
                Some(items) if !items.is_empty() => {
                    if self.apply_target.as_ref().is_none_or(|k| !items.iter().any(|i| &i.key == k)) {
                        self.apply_target = Some(items[0].key.clone());
                    }
                    let current = self.apply_target.clone();
                    let opts: Vec<_> = items.iter().map(|i| Some(i.key.clone())).collect();
                    let r = egui::ComboBox::from_id_salt("apply_target")
                        .selected_text(current.as_ref().map(|k| k.label()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for item in items {
                                let value = item.value.map(|c| c.hex()).unwrap_or_else(|| "透明".into());
                                let text = format!("{}（{}）", item.key.label(), value);
                                if ui.selectable_label(current.as_ref() == Some(&item.key), text).clicked() {
                                    self.apply_target = Some(item.key.clone());
                                }
                            }
                        })
                        .response;
                    crate::egui_input::wheel_select(ui, &r, &mut self.apply_target, &opts);
                    let can_apply = self.selected.is_some() && self.apply_target.is_some();
                    if ui
                        .add_enabled(can_apply, egui::Button::new("選択色を適用"))
                        .on_hover_text(
                            "フォーカス中のオブジェクトの指定した色項目へ書き込む（複数選んでいても、書き込むのはフォーカス中の 1 つだけ）\nAviUtl2 本体の元に戻すで取り消せる",
                        )
                        .clicked()
                    {
                        if let (Some(c), Some(k)) = (self.selected, self.apply_target.clone()) {
                            self.apply(c, &k);
                        }
                    }
                }
                Some(_) => {
                    ui.small("フォーカス中のオブジェクトに色の項目がありません");
                }
                None => {
                    ui.small("フォーカス中のオブジェクトがありません");
                }
            }
        });

        let (status, status_kind, list_name, save_blocked) = {
            let s = self.shared.read();
            (s.status.clone(), s.status_kind, s.store.active_list().name.clone(), s.save_blocked)
        };
        if save_blocked {
            // 状態の欄は次の操作で上書きされるので、保存しないことは別に出し続ける
            ui_theme::status_label(
                ui,
                StatusKind::Warn,
                "履歴ファイルを読めなかったので、この起動の間は履歴を保存しません。履歴ファイルを確認してから AviUtl2 を再起動してください（詳しくはログ）",
            );
        }
        if !status.is_empty() {
            ui_theme::status_label(ui, status_kind, status);
        }
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.small(format!("記録先: {list_name}"));
            });
        });
    }

    fn run_search(&mut self) {
        let Some(target) = Rgb::parse(&self.replace.find) else {
            self.say(
                StatusKind::Error,
                "探す色をカラーコードで読めません。rrggbb の形で入れるか、「選択色」を押してください",
            );
            return;
        };
        let metric = self.shared.read().store.settings.replace_metric;
        let query = SearchQuery {
            target,
            tolerance: self.replace.tolerance.min(TOLERANCE_MAX),
            metric,
            include_text: self.replace.include_text,
            all_scenes: self.replace.all_scenes,
        };
        let project = self.shared.read().project_path.clone();
        // 検索は読み取りだけ（本体の Undo に触れない）
        match edit_ops::catch_panic(|| replace::search(&query, project.as_deref())) {
            Ok(result) => {
                let editable = result.hits.iter().filter(|h| h.editable()).count();
                self.replace.checked = result.hits.iter().map(|h| h.editable()).collect();
                self.set_status(format!(
                    "{} 件見つかりました（置換できる表示中のシーン: {editable} 件）",
                    result.hits.len()
                ));
                self.replace.hits = result.hits;
                self.replace.notes = result.notes;
                self.replace.query = Some(query);
            }
            Err(e) => {
                self.replace.hits.clear();
                self.replace.checked.clear();
                self.replace.query = None;
                self.say(StatusKind::Error, format!("検索できませんでした: {e}。プロジェクトを開いた状態で、もう一度検索してください"));
            }
        }
    }

    fn run_replace(&mut self) {
        let (Some(query), Some(new)) = (self.replace.query, Rgb::parse(&self.replace.to)) else {
            self.say(
                StatusKind::Error,
                "置換後の色をカラーコードで読めません。rrggbb の形で入れるか、「選択色」を押してください、または先に検索してください",
            );
            return;
        };
        let targets: Vec<Hit> = self
            .replace
            .hits
            .iter()
            .zip(&self.replace.checked)
            .filter(|(h, c)| **c && h.editable())
            .map(|(h, _)| h.clone())
            .collect();
        if targets.is_empty() {
            return;
        }
        // ボタン操作を起点に、1 回の編集セクションでまとめて書く（本体の元に戻す 1 回で戻る）
        match edit_ops::catch_panic(|| replace::apply(targets, &query, new)) {
            Ok(report) => {
                let mut msg = format!("{} 件を {} に置換しました（本体の元に戻す 1 回で戻せます）", report.replaced, new.hex());
                if report.skipped > 0 {
                    msg.push_str(&format!("／検索後に変わっていた {} 件は触っていません", report.skipped));
                }
                if !report.errors.is_empty() {
                    msg.push_str(&format!("／失敗 {} 件: {}", report.errors.len(), report.errors.join("、")));
                }
                if report.replaced > 0 {
                    let mut s = self.shared.write();
                    s.store.record(new, "一括置換", now_secs());
                    s.mark_dirty();
                }
                let kind = if !report.errors.is_empty() || report.skipped > 0 { StatusKind::Warn } else { StatusKind::Ok };
                // 結果を読み直してから、置換の報告を出す（検索の件数表示で上書きしない）
                self.run_search();
                self.say(kind, msg);
            }
            Err(e) => self.say(StatusKind::Error, format!("置換できませんでした: {e}。もう一度検索してから置換してください")),
        }
    }

    fn render_replace(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("一括置換");
            ui.label("探す色");
            // Enter で検索する（検索は読み取りだけ）。入力中の Esc で入力前の文字に戻す
            let find = egui_input::text_field(ui, &mut self.replace.find, |t| t.hint_text("rrggbb").desired_width(80.0));
            let find_enter = find.submitted;
            if ui.add_enabled(self.selected.is_some(), egui::Button::new("選択色")).clicked() {
                if let Some(c) = self.selected {
                    self.replace.find = c.hex();
                }
            }
            small_swatch(ui, Rgb::parse(&self.replace.find));
            ui.label("しきい値 ±");
            // 検索の条件に使うだけ（本体へは書かない）ので、ドラッグ中の値をそのまま使ってよい
            egui_input::number_field(ui, &mut self.replace.tolerance, &tolerance_spec()).response.on_hover_text(
                "8bit の段数。0 は完全一致、1 なら 1 だけ違う色もまとめる\n「各成分の差」では R・G・B のどの差もこの値以下の色\nほかの色差では、各成分の差でまとめられた色をすべて含む広さ（色差は見た目に近い形に変わる）",
            );
            ui.label("色差");
            let mut metric = self.shared.read().store.settings.replace_metric;
            let before = metric;
            let r = egui::ComboBox::from_id_salt("replace_metric")
                .selected_text(metric.label())
                .width(90.0)
                .show_ui(ui, |ui| {
                    for m in ColorMetric::ALL {
                        ui.selectable_value(&mut metric, m, m.label()).on_hover_text(m.hint());
                    }
                })
                .response;
            crate::egui_input::wheel_select(ui, &r, &mut metric, &ColorMetric::ALL);
            r
                .on_hover_text(format!("{}\n変えたら、もう一度検索する（置換は検索したときの色差で判定する）", metric.hint()));
            if metric != before {
                let mut s = self.shared.write();
                s.store.settings.replace_metric = metric;
                s.mark_dirty();
            }
            ui.checkbox(&mut self.replace.all_scenes, "全シーン")
                .on_hover_text("他のシーンは最後に保存した .aup2 から探す（未保存の変更は反映されない）。置換できるのは表示中のシーンだけ");
            ui.checkbox(&mut self.replace.include_text, "テキスト内の <#rrggbb>");
            if ui.button("検索").clicked() || find_enter {
                self.run_search();
            }
        });

        if self.replace.query.is_some() {
            for note in &self.replace.notes {
                ui.small(note);
            }
            let hits_len = self.replace.hits.len();
            ui.horizontal(|ui| {
                ui.small(format!("{hits_len} 件"));
                if ui_theme::small_button(ui, "すべて選択").clicked() {
                    for (c, h) in self.replace.checked.iter_mut().zip(&self.replace.hits) {
                        *c = h.editable();
                    }
                }
                if ui_theme::small_button(ui, "選択解除").clicked() {
                    self.replace.checked.iter_mut().for_each(|c| *c = false);
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("replace_hits")
                .max_height((ui.available_height() - 36.0).max(40.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, hit) in self.replace.hits.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let mut checked = self.replace.checked[i];
                            if ui.add_enabled(hit.editable(), egui::Checkbox::without_text(&mut checked)).changed() {
                                self.replace.checked[i] = checked;
                            }
                            for c in &hit.found {
                                small_swatch(ui, Some(*c));
                            }
                            let colors: Vec<String> = hit.found.iter().map(|c| c.hex()).collect();
                            let text = format!(
                                "{}  L{}  {}〜{}f  {}  {}",
                                hit.scene_name,
                                hit.layer + 1,
                                hit.frame_start + 1,
                                hit.frame_end + 1,
                                hit.place(),
                                colors.join(",")
                            );
                            // 文字を押してもチェックを切り替えられる（チェックの箱だけでは当たり判定が小さい）
                            let label = if hit.editable() {
                                ui.add(egui::Label::new(text).sense(egui::Sense::click()))
                            } else {
                                ui.label(egui::RichText::new(format!("{text}（保存データ・置換不可）")).weak())
                            };
                            if hit.editable() && label.clicked() {
                                self.replace.checked[i] = !self.replace.checked[i];
                            }
                        });
                    }
                });
        }

        ui.horizontal_wrapped(|ui| {
            ui.label("置換後の色");
            // 置換はボタンを押したときだけ（Enter では本体へ書かない）。入力中の Esc で入力前の文字に戻す
            egui_input::text_field(ui, &mut self.replace.to, |t| t.hint_text("rrggbb").desired_width(80.0));
            if ui.add_enabled(self.selected.is_some(), egui::Button::new("選択色")).clicked() {
                if let Some(c) = self.selected {
                    self.replace.to = c.hex();
                }
            }
            small_swatch(ui, Rgb::parse(&self.replace.to));
            let count = self
                .replace
                .hits
                .iter()
                .zip(&self.replace.checked)
                .filter(|(h, c)| **c && h.editable())
                .count();
            let ready = count > 0 && Rgb::parse(&self.replace.to).is_some();
            // 何件の何を・どこへ書くかをボタンに出す。本体の元に戻す 1 回で戻せるので、確かめの窓は出さない
            let button_text = match Rgb::parse(&self.replace.to) {
                Some(new) => format!("チェックした {count} 件を {} に置換", new.hex()),
                None => format!("チェックした {count} 件を置換"),
            };
            if ui
                .add_enabled(ready, egui::Button::new(button_text))
                .on_hover_text(format!(
                    "{count} 件の色を、表示中のシーンのオブジェクトに書く（本体の元に戻す 1 回で戻せる）\n置換の直前に値を読み直し、まだ条件に合う項目だけを書き換える"
                ))
                .clicked()
            {
                self.run_replace();
            }
        });
    }

    /// 並べ替えた表示を、履歴・設定（`revision`）か検索語が変わったときだけ作り直す。
    /// 色相順は色ごとに変換が要るので、1000 色を超える履歴でフレームごとに並べ直さない
    fn refresh_view(&mut self) {
        let s = self.shared.read();
        if self.view_cache.as_ref().is_some_and(|v| v.revision == s.revision && v.search == self.search_query) {
            return;
        }
        let list = s.store.active_list();
        let (pins, rest) = list.view(s.store.settings.sort, s.store.settings.hue_space, &self.search_query);
        let cache = ViewCache {
            revision: s.revision,
            search: self.search_query.clone(),
            pins: pins.into_iter().cloned().collect(),
            rest: rest.into_iter().cloned().collect(),
            other_lists: s.store.lists.iter().filter(|l| l.id != list.id).map(|l| (l.id.clone(), l.name.clone())).collect(),
            list_name: list.name.clone(),
            size: s.store.settings.swatch_size.clamp(SWATCH_MIN, SWATCH_MAX),
            copy_format: s.store.settings.copy_format,
        };
        drop(s);
        self.view_cache = Some(cache);
    }

    #[allow(clippy::too_many_arguments)]
    fn swatch(
        &self,
        ui: &mut egui::Ui,
        entry: &Entry,
        size: f32,
        copy_format: CopyFormat,
        focused: &Option<Vec<ColorItem>>,
        other_lists: &[(String, String)],
        actions: &mut Vec<Action>,
    ) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
        let painter = ui.painter();
        let inner = rect.shrink(1.0);
        painter.rect_filled(inner, 3.0, colors::from_data(entry.color));
        let selected = self.selected == Some(entry.color);
        let stroke = if selected {
            egui::Stroke::new(2.5, colors::SWATCH_SELECTED_STROKE)
        } else if response.hovered() {
            egui::Stroke::new(1.5, colors::SWATCH_HOVER_STROKE)
        } else {
            egui::Stroke::new(1.0, colors::SWATCH_STROKE)
        };
        painter.rect_stroke(inner, 3.0, stroke, egui::StrokeKind::Inside);
        let mark = if entry.color.luminance() > 140.0 {
            colors::MARK_ON_LIGHT
        } else {
            colors::MARK_ON_DARK
        };
        if entry.pinned {
            painter.circle_filled(rect.right_top() + egui::vec2(-5.0, 5.0), 2.5, mark);
        }
        if !entry.label.is_empty() {
            painter.line_segment(
                [rect.left_bottom() + egui::vec2(4.0, -4.0), rect.left_bottom() + egui::vec2(10.0, -4.0)],
                egui::Stroke::new(1.5, mark),
            );
        }

        let now = now_secs();
        let response = response.on_hover_ui(|ui| {
            let (h, s, v) = entry.color.hsv();
            let [ol, oc, oh] = crate::colorspace::oklch(entry.color);
            let title = if entry.label.is_empty() {
                entry.color.hex()
            } else {
                format!("{}  {}", entry.color.hex(), entry.label)
            };
            ui.strong(title);
            ui.label(format!(
                "RGB {}, {}, {}   HSV {:.0}°, {:.0}%, {:.0}%",
                entry.color.r, entry.color.g, entry.color.b, h, s * 100.0, v * 100.0
            ));
            ui.label(format!("Oklch {:.1}%, {:.3}, {:.0}°", ol * 100.0, oc, oh));
            ui.label(format!("{} 回使用・最後に使ったのは {}", entry.use_count, elapsed_text(entry.last_used, now)));
            if !entry.last_source.is_empty() {
                ui.label(format!("場所: {}", entry.last_source));
            }
            ui.small(format!("クリック: {} をコピー / 右クリック: メニュー", copy_format.format(entry.color)));
        });

        if response.clicked() {
            actions.push(Action::Copy(entry.color, copy_format));
        }
        response.context_menu(|ui| {
            actions.push(Action::Select(entry.color));
            ui.label(egui::RichText::new(entry.color.hex()).strong());
            ui.separator();
            for f in CopyFormat::ALL {
                if ui.button(format!("コピー: {}", f.format(entry.color))).clicked() {
                    actions.push(Action::Copy(entry.color, f));
                    ui.close();
                }
            }
            ui.separator();
            if let Some(items) = focused.as_ref().filter(|v| !v.is_empty()) {
                ui.menu_button("フォーカス中のオブジェクトへ適用", |ui| {
                    for item in items {
                        if ui.button(item.key.label()).clicked() {
                            actions.push(Action::Apply(entry.color, item.key.clone()));
                            ui.close();
                        }
                    }
                });
            }
            if !other_lists.is_empty() {
                ui.menu_button("リストへコピー", |ui| {
                    for (id, name) in other_lists {
                        if ui.button(name).clicked() {
                            actions.push(Action::CopyToList(entry.color, id.clone()));
                            ui.close();
                        }
                    }
                });
            }
            let pin_label = if entry.pinned { "ピン留めを外す" } else { "ピン留め" };
            if ui.button(pin_label).clicked() {
                actions.push(Action::SetPinned(entry.color, !entry.pinned));
                ui.close();
            }
            if ui.button("ラベルを編集").clicked() {
                actions.push(Action::StartLabel(entry.color));
                ui.close();
            }
            if ui.button("このリストから消す").clicked() {
                actions.push(Action::Delete(entry.color));
                ui.close();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_grid(
        &self,
        ui: &mut egui::Ui,
        entries: &[Entry],
        size: f32,
        copy_format: CopyFormat,
        focused: &Option<Vec<ColorItem>>,
        other_lists: &[(String, String)],
        actions: &mut Vec<Action>,
        with_labels: bool,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for entry in entries {
                if with_labels && !entry.label.is_empty() {
                    ui.horizontal(|ui| {
                        self.swatch(ui, entry, size, copy_format, focused, other_lists, actions);
                        ui.small(&entry.label);
                    });
                } else {
                    self.swatch(ui, entry, size, copy_format, focused, other_lists, actions);
                }
            }
        });
    }
}

impl eframe::App for ColorHistoryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 本体のフォントは窓が見えてから読む。読んだフレームは描かない（新しいフォントは次のフレームから効く。次のフレームは頼んである）
        if self.host_fonts.load_if_visible(ui.ctx()) {
            return;
        }
        // IME の確定の Enter を、実行・確定に使わない
        egui_input::drop_ime_enter(ui.ctx());
        // 押せる所の高さを 24px 以上に（ボタン・トグル・コンボボックス・チェックの当たり判定）
        ui.spacing_mut().interact_size.y = ui_theme::MIN_HIT;
        // 本体がプロジェクトを開いて記録先が変わっても、窓が止まっていると表示が古いままになる（切り替わらないと言われた）。
        // 遅くとも 0.5 秒後には描き直し、見えているリストが変わったらログに残す（見えていたかを後から追えるように）
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        {
            let active = self.shared.read().store.active.clone();
            if self.shown_active != active {
                tracing::info!("ColorHistory_H: 窓の表示を記録先「{active}」に切り替えた");
                self.shown_active = active;
            }
        }
        self.update_picking(ui.ctx());
        self.handle_shortcuts(ui.ctx());

        egui::Panel::top("top").show(ui, |ui| {
            self.render_top(ui);
        });

        // 先に足したパネルほど外側になる。ステータスと置換は横幅いっぱい、リストはその内側に置く
        let focused = self.shared.read().focused.clone();
        egui::Panel::bottom("bottom").show(ui, |ui| {
            self.render_bottom(ui, &focused);
        });

        if self.replace_open {
            egui::Panel::bottom("replace")
                .resizable(true)
                .default_size(200.0)
                .min_size(90.0)
                .show(ui, |ui| {
                    self.render_replace(ui);
                });
        }

        if self.lists_open {
            egui::Panel::left("lists")
                .default_size(130.0)
                .resizable(true)
                .show(ui, |ui| {
                    self.render_lists(ui);
                });
        }

        self.refresh_view();
        let Some(view) = self.view_cache.as_ref() else {
            return;
        };
        let (pins, rest, size, copy_format, other_lists, list_name) =
            (&view.pins, &view.rest, view.size, view.copy_format, &view.other_lists, &view.list_name);

        // 中央パネルは最後に追加する（先に足すと後から足したパネルが後ろに回り込む）
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !pins.is_empty() {
                    ui.label(egui::RichText::new(format!("ピン留め（{}）", pins.len())).strong());
                    self.render_grid(ui, pins, size, copy_format, &focused, other_lists, &mut actions, true);
                    ui.add_space(6.0);
                }
                ui.label(egui::RichText::new(format!("{list_name}（{}）", rest.len())).strong());
                if rest.is_empty() && pins.is_empty() {
                    if self.search_query.trim().is_empty() {
                        ui.small("まだ色がありません。オブジェクトの色を変える・スポイトで拾うと、ここに記録されます。");
                    } else {
                        ui.small("一致する色がありません");
                    }
                } else {
                    self.render_grid(ui, rest, size, copy_format, &focused, other_lists, &mut actions, false);
                }
            });
        });

        if !actions.is_empty() {
            self.run_actions(actions);
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    //! 画面の部品を egui だけで動かし、確定・取り消し・壊す操作の表示を確かめる

    use super::*;

    /// 画面なしで 1 フレーム回す。出力の textures_delta を空にしてから捨てる
    /// （そのまま捨てると、デバッグビルドで epaint の debug_assert「Dropped TexturesDelta with N unapplied deltas」に落ちる）
    fn run<R>(ctx: &egui::Context, events: Vec<egui::Event>, mut f: impl FnMut(&mut egui::Ui) -> R) -> R {
        let mut out = None;
        let mut full = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| out = Some(f(ui)));
        full.textures_delta.clear();
        out.unwrap()
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn select_all() -> egui::Event {
        egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }
    }

    /// 数値の欄に Tab でフォーカスし、全選択して `typed` を打つ（まだ確定しない）。打つ間に「確定」が出たら false
    fn type_number<T: egui::emath::Numeric + Send + Sync>(ctx: &egui::Context, v: &mut T, spec: &NumberSpec, typed: &str) -> bool {
        let mut any_commit = false;
        let draw = |ctx: &egui::Context, v: &mut T, events: Vec<egui::Event>| {
            run(ctx, events, |ui| egui_input::number_field(ui, v, spec)).committed
        };
        any_commit |= draw(ctx, v, vec![]);
        any_commit |= draw(ctx, v, vec![key(egui::Key::Tab)]);
        any_commit |= draw(ctx, v, vec![select_all()]);
        any_commit |= draw(ctx, v, vec![egui::Event::Text(typed.into())]);
        !any_commit
    }

    /// 打つ → Esc → 3 フレームのあいだ値は入力前のまま・確定しない。もう一度打って Enter で確定する
    fn check_number<T>(start: T, spec: &NumberSpec, typed: &str, expected: T)
    where
        T: egui::emath::Numeric + Send + Sync + std::fmt::Debug,
    {
        let ctx = egui::Context::default();
        let mut v = start;
        assert!(type_number(&ctx, &mut v, spec, typed), "打っている途中で確定している");
        assert_eq!(v, start, "打っている途中の値を使っている");
        let esc = run(&ctx, vec![key(egui::Key::Escape)], |ui| egui_input::number_field(ui, &mut v, spec));
        assert!(!esc.committed);
        for _ in 0..3 {
            let o = run(&ctx, vec![], |ui| egui_input::number_field(ui, &mut v, spec));
            assert_eq!(v, start, "Esc の後も入力前の値のまま");
            assert!(!o.committed, "Esc で取り消したのに確定した");
        }

        assert!(type_number(&ctx, &mut v, spec, typed));
        let mut committed = run(&ctx, vec![key(egui::Key::Enter)], |ui| egui_input::number_field(ui, &mut v, spec)).committed;
        for _ in 0..3 {
            committed |= run(&ctx, vec![], |ui| egui_input::number_field(ui, &mut v, spec)).committed;
        }
        assert!(committed, "Enter で確定したのに committed にならない");
        assert_eq!(v, expected, "Enter で確定した値は残る");
    }

    /// 件数の上限（usize）。確定したときだけ本体の設定へ反映する
    #[test]
    fn max_entries_escape_discards_and_enter_commits() {
        check_number::<usize>(500, &max_entries_spec(), "1234", 1234);
    }

    /// しきい値 ±（u8）
    #[test]
    fn tolerance_escape_discards_and_enter_commits() {
        check_number::<u8>(0, &tolerance_spec(), "12", 12);
    }

    #[test]
    fn number_specs_keep_old_ranges_and_defaults() {
        let m = max_entries_spec();
        assert_eq!((*m.hard.start(), *m.hard.end(), m.default), (16.0, 10_000.0, Some(500.0)));
        let t = tolerance_spec();
        assert_eq!((*t.hard.start(), *t.hard.end(), t.default), (0.0, 64.0, Some(0.0)));
    }

    /// 編集欄と、ほかの欄（フォーカスを移す先）を 1 フレーム描く
    fn draw_line(ctx: &egui::Context, t: &mut String, events: Vec<egui::Event>) -> Option<bool> {
        run(ctx, events, |ui| {
            let r = edit_line(ui, t, 100.0, false);
            let mut other = String::new();
            ui.text_edit_singleline(&mut other);
            r
        })
    }

    /// 編集欄にフォーカスを移した状態の ctx を作る
    fn focused_line(t: &mut String) -> egui::Context {
        let ctx = egui::Context::default();
        draw_line(&ctx, t, vec![]);
        draw_line(&ctx, t, vec![key(egui::Key::Tab)]);
        ctx
    }

    /// ラベル・リスト名の編集欄: 打って Enter で確定、Esc でやめる、ほかをクリックで確定
    #[test]
    fn edit_line_enter_commits_escape_cancels_blur_commits() {
        let mut t = String::from("赤");
        let ctx = focused_line(&mut t);
        assert_eq!(draw_line(&ctx, &mut t, vec![egui::Event::Text("青".into())]), None, "打っている途中は終わらない");
        assert_eq!(draw_line(&ctx, &mut t, vec![key(egui::Key::Enter)]), Some(true));
        assert_eq!(t, "赤青");

        // 打って Esc → 入力前の文字に戻り、やめる
        let mut t = String::from("赤");
        let ctx = focused_line(&mut t);
        draw_line(&ctx, &mut t, vec![egui::Event::Text("青".into())]);
        assert_eq!(draw_line(&ctx, &mut t, vec![key(egui::Key::Escape)]), Some(false));
        assert_eq!(t, "赤", "Esc で入力前の文字に戻る");

        // 打ってほかの欄へ移る（クリックと同じくフォーカスが外れる）→ 確定
        let mut t = String::from("赤");
        let ctx = focused_line(&mut t);
        draw_line(&ctx, &mut t, vec![egui::Event::Text("青".into())]);
        assert_eq!(draw_line(&ctx, &mut t, vec![key(egui::Key::Tab)]), Some(true));
        assert_eq!(t, "赤青");
    }

    #[test]
    fn clear_button_shows_count_and_target() {
        assert_eq!(clear_button_label("既定", 12, false), "「既定」のピン留め以外 12 件を消去");
        assert!(clear_button_label("既定", 12, true).starts_with("もう一度押すと、「既定」のピン留め以外 12 件"));
    }

    #[test]
    fn total_entries_counts_all_lists() {
        let mut store = crate::history::Store::default();
        store.record(Rgb { r: 1, g: 2, b: 3 }, "a", 1);
        let other = store.create_list("他", 2);
        store.set_active(&other);
        store.record(Rgb { r: 4, g: 5, b: 6 }, "b", 3);
        assert_eq!(total_entries(&store), 2);
    }

    #[test]
    fn swatch_is_big_enough_to_hit() {
        assert!(SWATCH_MIN >= ui_theme::MIN_HIT);
    }
}
