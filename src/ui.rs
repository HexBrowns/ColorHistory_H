//! 色履歴ウィンドウ（egui）

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use aviutl2_eframe::{eframe, egui, AviUtl2EframeHandle};

use crate::color::{CopyFormat, Rgb};
use crate::history::{now_secs, Entry, SortMode, DEFAULT_LIST_ID, MAX_ENTRIES_MAX, MAX_ENTRIES_MIN};
use crate::input::{self, LineEnd};
use crate::replace::{self, Hit, SearchQuery, TOLERANCE_MAX};
use crate::tracker::{ColorItem, ColorItemKey};
use crate::watcher::Msg;
use crate::{clipboard, edit_ops, eyedropper, SharedState};

const SWATCH_MIN: f32 = 16.0;
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

pub struct ColorHistoryApp {
    _handle: AviUtl2EframeHandle,
    shared: SharedState,
    watcher: Option<Sender<Msg>>,
    search: String,
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
}

fn color32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
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

/// 数値欄（ルール au2-rs-plugin「入力の確定と取り消し」）。`configure` で範囲・速さを付けた `DragValue` に
/// `update_while_editing(false)` を足して置く。打っている途中の値は使わず、Enter かほかをクリックで確定する。
///
/// egui 0.36.2 の `DragValue` は `update_while_editing(false)` でも、Esc の次のフレームで打った文字を値にしてしまう
/// （フォーカスを失ったとみなす期間が 2 フレームあり、2 フレーム目には Esc が押されていないため）。
/// Esc のフレームの値を覚えておき、次のフレームで戻す（参照実装 LayerSilenceCut_H の `setting_value`）。
/// 戻したフレームは、欄を描く前と値が同じなら `changed()` を偽にする（件数の上限で保存を走らせない）
fn drag_value<T>(
    ui: &mut egui::Ui,
    value: &mut T,
    configure: impl for<'v> FnOnce(egui::DragValue<'v>) -> egui::DragValue<'v>,
) -> egui::Response
where
    T: egui::emath::Numeric + Default + Send + Sync,
{
    let before = *value;
    let mut resp = ui.add(configure(egui::DragValue::new(&mut *value)).update_while_editing(false));
    let key = resp.id.with("escaped");
    let pass = ui.ctx().cumulative_pass_nr();
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(key, (pass, *value)));
    } else if let Some((at, kept)) = ui.data_mut(|d| d.remove_temp::<(u64, T)>(key)) {
        if pass == at + 1 {
            *value = kept;
            if *value == before {
                // `flags` は egui の doc(hidden) の公開フィールド。`changed()` を外す手段がこれしかない（0.36.2）
                resp.flags.remove(egui::response::Flags::CHANGED);
            }
        }
    }
    resp
}

fn small_swatch(ui: &mut egui::Ui, c: Option<Rgb>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    match c {
        Some(c) => {
            ui.painter().rect_filled(rect, 2.0, color32(c));
        }
        None => {
            ui.painter()
                .rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Inside);
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
        cc.egui_ctx.set_fonts(aviutl2_eframe::aviutl2_fonts());
        shared.write().egui_ctx = Some(cc.egui_ctx.clone());
        Self {
            _handle: handle,
            shared,
            watcher,
            search: String::new(),
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
        }
    }

    fn set_status(&self, msg: impl Into<String>) {
        self.shared.write().status = msg.into();
    }

    fn copy(&self, color: Rgb, format: CopyFormat) {
        let text = format.format(color);
        match clipboard::set_text(&text) {
            Ok(()) => self.set_status(format!("コピーしました: {text}")),
            Err(e) => self.set_status(format!("コピーできませんでした: {e}")),
        }
    }

    fn apply(&mut self, color: Rgb, key: &ColorItemKey) {
        // ボタン操作を起点にしたときだけ書き込む（イベントからは書かない）
        match edit_ops::catch_panic(|| edit_ops::apply_color(key, color)) {
            Ok(()) => self.set_status(format!("{} に {} を適用しました", key.label(), color.hex())),
            Err(e) => self.set_status(e),
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
                        s.status = format!("{} をこのリストから消しました", c.hex());
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
                        s.status = format!("{} を「{name}」へコピーしました", c.hex());
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
                s.status = format!("画面の色 {} を記録しました", c.hex());
                drop(s);
                self.selected = Some(c);
            }
            None => self.set_status("画面の色を読めませんでした"),
        }
    }

    fn render_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.lists_open, "リスト");
            // 打つたびに絞り込む（軽い）。入力中の Esc で入力前の文字に戻す
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("検索: カラーコード・ラベル")
                    .desired_width(ui.available_width().min(150.0)),
            );
            input::track(ui, &search, &mut self.search);
            let mut s = self.shared.write();
            let mut sort = s.store.settings.sort;
            egui::ComboBox::from_id_salt("sort")
                .selected_text(sort.label())
                .show_ui(ui, |ui| {
                    for m in SortMode::ALL {
                        ui.selectable_value(&mut sort, m, m.label());
                    }
                });
            if sort != s.store.settings.sort {
                s.store.settings.sort = sort;
                s.mark_dirty();
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
                    None => self.set_status("監視スレッドが動いていません"),
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
            if drag_value(ui, &mut max, |d| d.range(MAX_ENTRIES_MIN..=MAX_ENTRIES_MAX).speed(2.0))
                .on_hover_text("1 つのリストでピン留めしていない色の最大件数。超えたら最後に使ったのが古い色から消える")
                .changed()
            {
                s.store.settings.max_entries = max;
                s.store.apply_limit();
                s.mark_dirty();
            }
            ui.label("見本の大きさ");
            if ui
                .add(egui::Slider::new(&mut s.store.settings.swatch_size, SWATCH_MIN..=SWATCH_MAX))
                .changed()
            {
                s.mark_dirty();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("クリックでコピーする形式");
            let mut format = s.store.settings.copy_format;
            egui::ComboBox::from_id_salt("copy_format")
                .selected_text(format.label())
                .show_ui(ui, |ui| {
                    for f in CopyFormat::ALL {
                        ui.selectable_value(&mut format, f, f.label());
                    }
                });
            if format != s.store.settings.copy_format {
                s.store.settings.copy_format = format;
                s.mark_dirty();
            }
        });
        drop(s);
        ui.horizontal_wrapped(|ui| {
            ui.label("色を追加");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.add_input)
                    .hint_text("ff8000 / #ff8000 / 255,128,0")
                    .desired_width(140.0),
            );
            // 追加するのは Enter かボタンのときだけ（ほかをクリックしただけでは追加しない）。Esc は入力前の文字に戻す
            let enter = input::track(ui, &response, &mut self.add_input).enter();
            if ui.button("追加").clicked() || enter {
                match Rgb::parse(&self.add_input) {
                    Some(c) => {
                        let mut s = self.shared.write();
                        s.store.record(c, "手入力", now_secs());
                        s.mark_dirty();
                        s.status = format!("{} を追加しました", c.hex());
                        drop(s);
                        self.selected = Some(c);
                        self.add_input.clear();
                    }
                    None => self.set_status("カラーコードとして読めません"),
                }
            }
            let armed = self.clear_armed_at.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
            let label = if armed { "もう一度押すと消去" } else { "このリストのピン留め以外を消去" };
            if ui.button(label).clicked() {
                if armed {
                    let mut s = self.shared.write();
                    let n = s.store.active_list_mut().clear_unpinned();
                    s.mark_dirty();
                    s.status = format!("{n} 件消去しました（ピン留めは残しています）");
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
                        let response = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
                        if !response.has_focus() && !response.lost_focus() {
                            response.request_focus();
                        }
                        // Enter か欄の外をクリックで確定、Esc で取り消し
                        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if escape {
                            self.list_rename = None;
                        } else if response.lost_focus() {
                            let new_name = text.clone();
                            let mut s = self.shared.write();
                            if s.store.rename_list(&rename_id, &new_name) {
                                s.mark_dirty();
                            }
                            drop(s);
                            self.list_rename = None;
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
                        s.status = format!("記録先を「{name}」にしました");
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
                    let label = if armed { "もう一度押すと削除" } else { "リストを削除" };
                    if ui.add_enabled(!is_default, egui::Button::new(label)).clicked() {
                        if armed {
                            let mut s = self.shared.write();
                            if s.store.delete_list(id).is_some() {
                                s.mark_dirty();
                                s.status = format!("リスト「{name}」を削除しました");
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
            s.status = format!("リスト「{name}」を作り、記録先にしました");
            drop(s);
            self.list_rename = Some((id, name));
            self.selected = None;
        }
    }

    fn render_bottom(&mut self, ui: &mut egui::Ui, focused: &Option<Vec<ColorItem>>) {
        if let Some((color, text)) = &mut self.label_edit {
            let color = *color;
            let mut done = None;
            let focus = std::mem::take(&mut self.label_focus);
            ui.horizontal(|ui| {
                ui.label(format!("{} のラベル", color.hex()));
                let response = ui.add(egui::TextEdit::singleline(text).desired_width(160.0));
                if focus {
                    response.request_focus();
                }
                // Enter で保存、入力中の Esc でやめる（ラベルは編集前のまま）。
                // ほかをクリックしただけでは保存しない（「やめる」を押すとき、先にフォーカスが外れるため）
                match input::track(ui, &response, text) {
                    LineEnd::Committed { enter: true, .. } => done = Some(true),
                    LineEnd::Reverted => done = Some(false),
                    _ => {}
                }
                if ui.button("保存").clicked() {
                    done = Some(true);
                }
                if ui.button("やめる").clicked() {
                    done = Some(false);
                }
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
                    egui::ComboBox::from_id_salt("apply_target")
                        .selected_text(current.as_ref().map(|k| k.label()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for item in items {
                                let value = item.value.map(|c| c.hex()).unwrap_or_else(|| "透明".into());
                                let text = format!("{}（{}）", item.key.label(), value);
                                if ui.selectable_label(current.as_ref() == Some(&item.key), text).clicked() {
                                    self.apply_target = Some(item.key.clone());
                                }
                            }
                        });
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

        let (status, list_name, save_blocked) = {
            let s = self.shared.read();
            (s.status.clone(), s.store.active_list().name.clone(), s.save_blocked)
        };
        if save_blocked {
            // 状態の欄は次の操作で上書きされるので、保存しないことは別に出し続ける
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "履歴ファイルを読めなかったので、この起動の間は履歴を保存しません（詳しくはログ）",
            );
        }
        ui.horizontal(|ui| {
            ui.small(status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.small(format!("記録先: {list_name}"));
            });
        });
    }

    fn run_search(&mut self) {
        let Some(target) = Rgb::parse(&self.replace.find) else {
            self.set_status("探す色をカラーコードで入れてください");
            return;
        };
        let query = SearchQuery {
            target,
            tolerance: self.replace.tolerance.min(TOLERANCE_MAX),
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
                self.set_status(format!("検索できませんでした: {e}"));
            }
        }
    }

    fn run_replace(&mut self) {
        let (Some(query), Some(new)) = (self.replace.query, Rgb::parse(&self.replace.to)) else {
            self.set_status("置換後の色をカラーコードで入れてください");
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
        match edit_ops::catch_panic(|| replace::apply(targets, query.target, query.tolerance, new)) {
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
                // 結果を読み直してから、置換の報告を出す（検索の件数表示で上書きしない）
                self.run_search();
                self.set_status(msg);
            }
            Err(e) => self.set_status(format!("置換できませんでした: {e}")),
        }
    }

    fn render_replace(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("一括置換");
            ui.label("探す色");
            // Enter で検索する（検索は読み取りだけ）。入力中の Esc で入力前の文字に戻す
            let find = ui.add(egui::TextEdit::singleline(&mut self.replace.find).hint_text("rrggbb").desired_width(80.0));
            let find_enter = input::track(ui, &find, &mut self.replace.find).enter();
            if ui.add_enabled(self.selected.is_some(), egui::Button::new("選択色")).clicked() {
                if let Some(c) = self.selected {
                    self.replace.find = c.hex();
                }
            }
            small_swatch(ui, Rgb::parse(&self.replace.find));
            ui.label("しきい値 ±");
            drag_value(ui, &mut self.replace.tolerance, |d| d.range(0..=TOLERANCE_MAX))
                .on_hover_text("RGB の各成分の差がこの値以下なら同じ色とみなす（0 は完全一致、1 なら 1 だけ違う色もまとめる）");
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
                if ui.small_button("すべて選択").clicked() {
                    for (c, h) in self.replace.checked.iter_mut().zip(&self.replace.hits) {
                        *c = h.editable();
                    }
                }
                if ui.small_button("選択解除").clicked() {
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
                            if hit.editable() {
                                ui.label(text);
                            } else {
                                ui.label(egui::RichText::new(format!("{text}（保存データ・置換不可）")).weak());
                            }
                        });
                    }
                });
        }

        ui.horizontal_wrapped(|ui| {
            ui.label("置換後の色");
            // 置換はボタンを押したときだけ（Enter では本体へ書かない）。入力中の Esc で入力前の文字に戻す
            let to = ui.add(egui::TextEdit::singleline(&mut self.replace.to).hint_text("rrggbb").desired_width(80.0));
            input::track(ui, &to, &mut self.replace.to);
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
            if ui
                .add_enabled(ready, egui::Button::new(format!("チェックした {count} 件を置換")))
                .on_hover_text("置換の直前に値を読み直し、まだ条件に合う項目だけを書き換える")
                .clicked()
            {
                self.run_replace();
            }
        });
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
        painter.rect_filled(inner, 3.0, color32(entry.color));
        let selected = self.selected == Some(entry.color);
        let stroke = if selected {
            egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 200, 80))
        } else if response.hovered() {
            egui::Stroke::new(1.5, egui::Color32::from_gray(210))
        } else {
            egui::Stroke::new(1.0, egui::Color32::from_gray(70))
        };
        painter.rect_stroke(inner, 3.0, stroke, egui::StrokeKind::Inside);
        let mark = if entry.color.luminance() > 140.0 {
            egui::Color32::from_gray(20)
        } else {
            egui::Color32::from_gray(235)
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

        let (pins, rest, size, copy_format, other_lists, list_name) = {
            let s = self.shared.read();
            let list = s.store.active_list();
            let (pins, rest) = list.view(s.store.settings.sort, &self.search);
            let other_lists: Vec<(String, String)> = s
                .store
                .lists
                .iter()
                .filter(|l| l.id != list.id)
                .map(|l| (l.id.clone(), l.name.clone()))
                .collect();
            (
                pins.into_iter().cloned().collect::<Vec<_>>(),
                rest.into_iter().cloned().collect::<Vec<_>>(),
                s.store.settings.swatch_size.clamp(SWATCH_MIN, SWATCH_MAX),
                s.store.settings.copy_format,
                other_lists,
                list.name.clone(),
            )
        };

        // 中央パネルは最後に追加する（先に足すと後から足したパネルが後ろに回り込む）
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !pins.is_empty() {
                    ui.label(egui::RichText::new(format!("ピン留め（{}）", pins.len())).strong());
                    self.render_grid(ui, &pins, size, copy_format, &focused, &other_lists, &mut actions, true);
                    ui.add_space(6.0);
                }
                ui.label(egui::RichText::new(format!("{list_name}（{}）", rest.len())).strong());
                if rest.is_empty() && pins.is_empty() {
                    if self.search.trim().is_empty() {
                        ui.small("まだ色がありません。オブジェクトの色を変える・スポイトで拾うと、ここに記録されます。");
                    } else {
                        ui.small("一致する色がありません");
                    }
                } else {
                    self.render_grid(ui, &rest, size, copy_format, &focused, &other_lists, &mut actions, false);
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
mod drag_value_tests {
    //! 数値欄を egui だけで動かし、打っている途中の値が使われないこと・Esc で戻ること・Enter で確定することを確かめる

    use super::*;

    /// 画面なしで 1 フレーム回す。出力の textures_delta を空にしてから捨てる
    /// （そのまま捨てると、デバッグビルドで epaint の debug_assert「Dropped TexturesDelta with N unapplied deltas」に落ちる）
    fn run_frame(ctx: &egui::Context, input: egui::RawInput, f: impl FnMut(&mut egui::Ui)) {
        let mut out = ctx.run_ui(input, f);
        out.textures_delta.clear();
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    /// 1 フレーム描き、`changed()` を返す。`focus` なら描く前に欄へフォーカスを移す（クリックと同じく全選択で編集に入る）
    fn frame<T>(ctx: &egui::Context, value: &mut T, range: (T, T), events: Vec<egui::Event>, focus: bool) -> bool
    where
        T: egui::emath::Numeric + Default + Send + Sync,
    {
        let id_key = egui::Id::new("drag_value_test_id");
        let mut changed = false;
        let input = egui::RawInput { events, ..Default::default() };
        run_frame(ctx, input, |ui| {
            if focus {
                if let Some(id) = ui.data(|d| d.get_temp::<egui::Id>(id_key)) {
                    ui.memory_mut(|m| m.request_focus(id));
                }
            }
            let resp = drag_value(ui, value, |d| d.range(range.0..=range.1));
            ui.data_mut(|d| d.insert_temp(id_key, resp.id));
            changed = resp.changed();
        });
        changed
    }

    /// 打つ（まだ確定しない）
    fn type_in<T>(ctx: &egui::Context, value: &mut T, range: (T, T), typed: &str)
    where
        T: egui::emath::Numeric + Default + Send + Sync,
    {
        frame(ctx, value, range, vec![], false);
        frame(ctx, value, range, vec![], true);
        frame(ctx, value, range, vec![egui::Event::Text(typed.into())], false);
    }

    /// 打つ → Esc → その後 3 フレーム、値は入力前のまま（`changed()` も偽）。Enter で確定した値は残る
    fn check<T>(start: T, range: (T, T), typed: &str, expected: T)
    where
        T: egui::emath::Numeric + Default + Send + Sync + std::fmt::Debug,
    {
        let ctx = egui::Context::default();
        let mut v = start;
        type_in(&ctx, &mut v, range, typed);
        assert_eq!(v, start, "打っている途中の値を使っている");
        assert!(!frame(&ctx, &mut v, range, vec![key(egui::Key::Escape)], false));
        for _ in 0..3 {
            let changed = frame(&ctx, &mut v, range, vec![], false);
            assert_eq!(v, start, "Esc の後のフレームでも入力前の値のまま");
            assert!(!changed, "戻したフレームを「変わった」にしない");
        }

        type_in(&ctx, &mut v, range, typed);
        assert!(frame(&ctx, &mut v, range, vec![key(egui::Key::Enter)], false));
        for _ in 0..3 {
            frame(&ctx, &mut v, range, vec![], false);
        }
        assert_eq!(v, expected, "Enter で確定した値は残る");
    }

    /// 件数の上限（usize）
    #[test]
    fn max_entries_escape_discards_and_enter_commits() {
        check::<usize>(500, (MAX_ENTRIES_MIN, MAX_ENTRIES_MAX), "1234", 1234);
    }

    /// しきい値 ±（u8）
    #[test]
    fn tolerance_escape_discards_and_enter_commits() {
        check::<u8>(0, (0, TOLERANCE_MAX), "12", 12);
    }
}
