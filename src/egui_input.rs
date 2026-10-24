// 生成物: 正本は AI/tools/plugin_common/egui_input.rs（開発用のリポジトリ）。ここを直さず、正本を直して
// `python AI/tools/sync_plugin_common.py` で写す（`--check` で同じかを確かめる）。
//! egui の入力欄の部品（共通の規約 `~/.claude/rules/common-ui.md`「入力欄の確定と取り消し」「検索と絞り込み」と、
//! `au2-rs-plugin`「入力の確定と取り消し」「数値の欄とノードの窓」を 1 か所にまとめたもの）。
//!
//! - 1 行の入力欄: Enter かほかをクリックで確定、入力中の Esc で入力前の文字に戻す（`text_field`・`revert_on_escape`）
//! - 打つたびに絞り込む検索欄: IME の変換中は絞り込みを止め、確定してから絞る（`search_field`・`track_ime`）。
//!   確定の Enter が同じフレームの Enter のキーと一緒に届くことがあるので、`drop_ime_enter` で捨てる
//! - 検索の比べ方: 全角と半角・大文字と小文字・ひらがなとカタカナをそろえ（`normalize_for_search`）、
//!   先頭の一致 → 語の頭の一致 → 途中の一致 → 離れた一致の順に点数を付ける（`match_score`）
//! - 数値の欄: ドラッグで変え、Shift で細かく（egui の既定）、Ctrl で刻みに合わせる。打って入れるときは数式（`3*2`・`10/5+4`）を受ける。
//!   ドラッグはやわらかい上限まで、打って入れるのは固い上限まで。右クリックで既定値に戻す。
//!   本体へ書くのは `committed` のときだけにする（1 回のドラッグを取り消し 1 回にまとめる）
//! - 小さな窓を Esc で閉じる合図（`EscapeToClose`）
//!
//! 写しは `python AI/tools/sync_plugin_common.py` が各拠点の `src/egui_input.rs` へ置く。写しを直さず、ここを直す。

#![allow(dead_code)]

use std::ops::RangeInclusive;

use aviutl2_eframe::egui;

// ---- IME ----

/// IME で変換中かを、このフレームの入力で更新する（変換中の文字があれば true、確定か取り消しで false）
pub fn ime_composing_after(events: &[egui::Event], composing: bool) -> bool {
    let mut composing = composing;
    for event in events {
        if let egui::Event::Ime(ime) = event {
            match ime {
                egui::ImeEvent::Preedit { text, .. } => composing = !text.is_empty(),
                egui::ImeEvent::Commit(_) => composing = false,
                _ => {}
            }
        }
    }
    composing
}

/// 入力欄ごとに IME の変換中かを覚えて返す。欄を描いた直後に呼ぶ（フォーカスが無ければ false）。
/// フォーカスが外れたフレームも見る（変換中の文字と Enter が同じフレームに届いて欄が外れたときも、変換中と答える）
pub fn track_ime(ui: &egui::Ui, response: &egui::Response) -> bool {
    let key = response.id.with("ime_composing");
    let prev = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let now = (response.has_focus() || response.lost_focus()) && ui.input(|i| ime_composing_after(&i.events, prev));
    ui.data_mut(|d| d.insert_temp(key, now));
    now
}

/// このフレームに IME の出来事があれば、同じフレームの Enter のキーと改行の文字を捨てる。捨てたら true。
/// 確定の Enter を、実行・選びの確定・改行として扱わないため。窓を描く前（`ui` の初め）に 1 回呼ぶ
pub fn drop_ime_enter(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| drop_ime_enter_events(&mut i.events))
}

/// `drop_ime_enter` の中身（テスト用に分けた）
pub fn drop_ime_enter_events(events: &mut Vec<egui::Event>) -> bool {
    if !events.iter().any(|e| matches!(e, egui::Event::Ime(_))) {
        return false;
    }
    let before = events.len();
    events.retain(|e| match e {
        egui::Event::Key { key: egui::Key::Enter, .. } => false,
        egui::Event::Text(t) => !matches!(t.as_str(), "\n" | "\r" | "\r\n"),
        _ => true,
    });
    events.len() != before
}

// ---- 1 行の入力欄 ----

/// 入力中の Esc で、入力を始める前の文字に戻す。欄を描いた直後に呼ぶ。戻したら true
pub fn revert_on_escape(ui: &egui::Ui, response: &egui::Response, text: &mut String) -> bool {
    let key = response.id.with("before_edit");
    if response.gained_focus() {
        ui.data_mut(|d| d.insert_temp(key, text.clone()));
    }
    if response.lost_focus() {
        let before = ui.data_mut(|d| d.remove_temp::<String>(key));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(b) = before {
                let changed = *text != b;
                *text = b;
                return changed;
            }
        }
    }
    false
}

/// 確定したか: Enter かほかをクリックしてフォーカスが外れ、Esc で外れたのでない
pub fn is_committed(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape))
}

pub struct TextFieldOutput {
    pub response: egui::Response,
    /// Enter かほかをクリックで確定した（値として使うのはこのときだけ）
    pub committed: bool,
    /// Enter で確定した（実行のボタンの代わりに使う）
    pub submitted: bool,
    /// Esc で入力前の文字に戻した
    pub reverted: bool,
}

/// 確定と取り消しを持つ 1 行の入力欄。`edit` に `TextEdit::singleline(text)` の設定（幅・ヒント・id）を渡す。
/// 打っている途中の文字は `text` に入るが、値として使うのは `committed` のときだけにする
pub fn text_field(ui: &mut egui::Ui, text: &mut String, style: impl FnOnce(egui::TextEdit<'_>) -> egui::TextEdit<'_>) -> TextFieldOutput {
    let response = ui.add(style(egui::TextEdit::singleline(text)));
    let composing = track_ime(ui, &response);
    let reverted = revert_on_escape(ui, &response, text);
    let committed = !composing && is_committed(ui, &response);
    let submitted = committed && ui.input(|i| i.key_pressed(egui::Key::Enter));
    TextFieldOutput { response, committed, submitted, reverted }
}

pub struct SearchFieldOutput {
    pub response: egui::Response,
    /// 絞り込みに使う文字。IME の変換中は、変換を始める前の文字のまま（変換中は絞らない）
    pub query: String,
    /// 絞り込みに使う文字が、このフレームで変わった
    pub query_changed: bool,
    /// Enter を押した（変換の確定の Enter は含まない）
    pub submitted: bool,
}

/// 打つたびに絞り込む検索欄。変換中は絞らず、入力中の Esc は入力前の文字に戻す。
/// `style` に `TextEdit` の設定（幅・ヒント・id）を渡す
pub fn search_field(
    ui: &mut egui::Ui,
    text: &mut String,
    style: impl FnOnce(egui::TextEdit<'_>) -> egui::TextEdit<'_>,
) -> SearchFieldOutput {
    let response = ui.add(style(egui::TextEdit::singleline(text)));
    let composing = track_ime(ui, &response);
    let reverted = revert_on_escape(ui, &response, text);
    let key = response.id.with("search_query");
    let prev: Option<String> = ui.data(|d| d.get_temp(key));
    let query = if composing { prev.clone().unwrap_or_default() } else { text.clone() };
    let query_changed = prev.as_deref() != Some(query.as_str()) && (prev.is_some() || !query.is_empty()) || reverted;
    ui.data_mut(|d| d.insert_temp(key, query.clone()));
    let submitted = !composing && response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    SearchFieldOutput { response, query, query_changed, submitted }
}

// ---- 検索の比べ方 ----

/// 検索で比べる前に、打った文字と項目の両方をそろえる:
/// 全角の英数記号と空白 → 半角、大文字 → 小文字、カタカナ・半角カナ → ひらがな（半角の濁点・半濁点は前の字と合わせる）
pub fn normalize_for_search(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        let c = match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{2212}' => '-',
            _ => c,
        };
        if let Some(kata) = halfwidth_katakana(c) {
            let kata = match chars.peek() {
                Some('\u{FF9E}') => voiced(kata).inspect(|_| {
                    chars.next();
                }),
                Some('\u{FF9F}') => semi_voiced(kata).inspect(|_| {
                    chars.next();
                }),
                _ => None,
            }
            .unwrap_or(kata);
            out.push(katakana_to_hiragana(kata));
            continue;
        }
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
        } else {
            for l in katakana_to_hiragana(c).to_lowercase() {
                out.push(l);
            }
        }
    }
    out
}

fn katakana_to_hiragana(c: char) -> char {
    match c {
        '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c,
    }
}

/// 半角カナ（U+FF61〜U+FF9D）を全角に。濁点・半濁点だけのもの（U+FF9E・U+FF9F）は全角の記号に
fn halfwidth_katakana(c: char) -> Option<char> {
    const TABLE: &str = "。「」、・ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン゛゜";
    let i = (c as u32).checked_sub(0xFF61)? as usize;
    TABLE.chars().nth(i)
}

fn voiced(c: char) -> Option<char> {
    match c {
        'ウ' => Some('ヴ'),
        'カ' | 'キ' | 'ク' | 'ケ' | 'コ' | 'サ' | 'シ' | 'ス' | 'セ' | 'ソ' | 'タ' | 'チ' | 'ツ' | 'テ' | 'ト' | 'ハ' | 'ヒ' | 'フ' | 'ヘ' | 'ホ' => {
            char::from_u32(c as u32 + 1)
        }
        _ => None,
    }
}

fn semi_voiced(c: char) -> Option<char> {
    match c {
        'ハ' | 'ヒ' | 'フ' | 'ヘ' | 'ホ' => char::from_u32(c as u32 + 2),
        _ => None,
    }
}

/// 語の区切りとみなす字（この字の次が「語の頭」）
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '_' | '-' | '/' | '\\' | '.' | ',' | ':' | '@' | '(' | ')' | '[' | ']' | '「' | '」' | '（' | '）' | '・' | '、')
}

/// 点数（大きいほど上）: 先頭の一致 3・語の頭の一致 2・途中の一致 1・離れた一致（字の順だけ合う）0。合わなければ None。
/// 両方とも `normalize_for_search` を通した文字で渡す。空白で区切った語はそれぞれが合うこと（点数は足す）。
/// 並べるときは安定な並べ替えで点数の大きい順にする（同じ点数のものは元の順のまま）。
/// 一覧を絞り込む欄（打った語を含むものだけを残す）には、離れた一致を出さない `filter_score` を使う
pub fn match_score(haystack: &str, needle: &str) -> Option<u32> {
    score_terms(haystack, needle, true)
}

/// 絞り込み用の点数: `match_score` から離れた一致（字の順だけ合う）を外したもの。語が続けて含まれるものだけが残る
pub fn filter_score(haystack: &str, needle: &str) -> Option<u32> {
    score_terms(haystack, needle, false)
}

fn score_terms(haystack: &str, needle: &str, scattered: bool) -> Option<u32> {
    let mut total = 0;
    let mut any = false;
    for term in needle.split_whitespace() {
        any = true;
        total += term_score(haystack, term, scattered)?;
    }
    if any { Some(total) } else { Some(0) }
}

fn term_score(haystack: &str, term: &str, scattered: bool) -> Option<u32> {
    if haystack.starts_with(term) {
        return Some(3);
    }
    let mut best = None;
    for (i, _) in haystack.match_indices(term) {
        let prev = haystack[..i].chars().next_back();
        let s = if prev.is_some_and(is_separator) { 2 } else { 1 };
        best = best.max(Some(s));
        if s == 2 {
            break;
        }
    }
    if best.is_some() || !scattered {
        return best;
    }
    let mut it = haystack.chars();
    term.chars().all(|t| it.any(|h| h == t)).then_some(0)
}

// ---- 数値の欄 ----

/// 数式を値にする（`+ - * /`・かっこ・符号・小数。全角の数字と記号・U+2212 のマイナスも受ける）。式でなければ None
pub fn eval_number(text: &str) -> Option<f64> {
    let s: Vec<char> = normalize_for_search(text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '×' => '*',
            '÷' => '/',
            _ => c,
        })
        .collect();
    if s.is_empty() {
        return None;
    }
    let mut p = Parser { s: &s, i: 0 };
    let v = p.expr()?;
    (p.i == s.len() && v.is_finite()).then_some(v)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.i += 1;
            let r = self.unary()?;
            v = if op == '*' { v * r } else { v / r };
        }
        Some(v)
    }
    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.unary()?)
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            '(' => {
                self.i += 1;
                let v = self.expr()?;
                (self.peek()? == ')').then(|| self.i += 1)?;
                Some(v)
            }
            _ => self.number(),
        }
    }
    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.i += 1;
        }
        if self.peek().is_some_and(|c| c == 'e') {
            let save = self.i;
            self.i += 1;
            if self.peek().is_some_and(|c| c == '-' || c == '+') {
                self.i += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i = save;
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let t: String = self.s[start..self.i].iter().collect();
        t.parse().ok()
    }
}

/// 数値の欄の決まり
#[derive(Clone)]
pub struct NumberSpec {
    /// 打って入れられる範囲（固い上限）
    pub hard: RangeInclusive<f64>,
    /// ドラッグで動く範囲（やわらかい上限）。None なら固い上限と同じ。打って範囲の外にした値からドラッグを始めたときは、その値まで広げる
    pub soft: Option<RangeInclusive<f64>>,
    /// ドラッグの速さ（1 画素あたりの量。Shift を押すと egui が細かくする）
    pub speed: f64,
    /// Ctrl を押しながらドラッグしたときに合わせる刻み
    pub snap: Option<f64>,
    /// 小数の桁の上限
    pub max_decimals: Option<usize>,
    /// 小数の桁の下限（`0.5000` のように桁をそろえて見せる）
    pub min_decimals: Option<usize>,
    pub suffix: String,
    /// 右クリックで戻す既定値
    pub default: Option<f64>,
    /// Ctrl+ホイール 1 段で動かす量。None なら刻み（`snap`）、無ければドラッグの速さ（`speed`）
    pub wheel_step: Option<f64>,
}

impl Default for NumberSpec {
    fn default() -> Self {
        Self {
            hard: f64::MIN..=f64::MAX,
            soft: None,
            speed: 1.0,
            snap: None,
            max_decimals: None,
            min_decimals: None,
            suffix: String::new(),
            default: None,
            wheel_step: None,
        }
    }
}

impl NumberSpec {
    pub fn new(hard: RangeInclusive<f64>, speed: f64) -> Self {
        Self { hard, speed, ..Self::default() }
    }
    pub fn soft(mut self, soft: RangeInclusive<f64>) -> Self {
        self.soft = Some(soft);
        self
    }
    pub fn snap(mut self, step: f64) -> Self {
        self.snap = Some(step);
        self
    }
    pub fn max_decimals(mut self, n: usize) -> Self {
        self.max_decimals = Some(n);
        self
    }
    /// 小数の桁を n 桁に固定する（上限と下限の両方）
    pub fn fixed_decimals(mut self, n: usize) -> Self {
        self.max_decimals = Some(n);
        self.min_decimals = Some(n);
        self
    }
    pub fn suffix(mut self, s: impl Into<String>) -> Self {
        self.suffix = s.into();
        self
    }
    pub fn default_value(mut self, v: f64) -> Self {
        self.default = Some(v);
        self
    }
    pub fn wheel_step(mut self, step: f64) -> Self {
        self.wheel_step = Some(step);
        self
    }
    /// Ctrl+ホイール 1 段の量（Shift で 10 分の 1。整数の欄（小数 0 桁）は細かくしない）
    fn wheel_amount(&self, fine: bool) -> f64 {
        let step = self.wheel_step.or(self.snap).unwrap_or(self.speed).abs();
        if fine && self.max_decimals != Some(0) {
            step / 10.0
        } else {
            step
        }
    }
}

pub struct NumberOutput {
    pub response: egui::Response,
    /// 本体やファイルへ書いてよい: ドラッグを離した（値が変わっていれば）・打って確定した・矢印キーで変えた・既定値に戻した・Ctrl+ホイールで変えた
    pub committed: bool,
    /// このフレームで値が変わった（ドラッグ中も含む。画面の中の見た目だけに使う）
    pub live: bool,
}

/// 数値の欄（`DragValue`）。打っている途中の値は使わず、Enter かほかをクリックで確定し、入力中の Esc で入力前の値に戻す。
///
/// egui 0.36.2 の `DragValue` は `update_while_editing(false)` でも、Esc の次のフレームで打った文字を値にしてしまう
/// （フォーカスを失ったとみなす期間が 2 フレームあり、2 フレーム目には Esc が押されていない）。
/// Esc のフレームの値を覚えておき、次のフレームで戻す（`au2-rs-plugin`「入力の確定と取り消し」）。
///
/// ドラッグ中の値は欄の一時領域にも持つ。呼ぶ側が毎フレーム本体やファイルの値から欄を作り直しても（確定するまで書かないので、
/// ドラッグ中は古い値のまま渡ってくる）、ドラッグが続き、離したときに確定する（EditHexus の設定パネルでドラッグが効かなかった。2026-10-24）。
///
/// 上に乗せて Ctrl+ホイールを回すと、上へ回すと増え、下へ回すと減る（1 段で `wheel_step`、Shift で 10 分の 1）。上限・下限で止め、すぐ確定する
/// （ユーザーの指示、2026-10-24。数値の欄を持つ自作プラグインは全部これを通す）。範囲はドラッグと同じ（やわらかい上限があればそちら）
pub fn number_field<T>(ui: &mut egui::Ui, value: &mut T, spec: &NumberSpec) -> NumberOutput
where
    T: egui::emath::Numeric + Send + Sync,
{
    // ドラッグ中の値を戻す（上の説明）。DragValue の id は、足す前の `next_auto_id`
    let live_key = ui.next_auto_id().with("number_field_live");
    let pass = ui.ctx().cumulative_pass_nr();
    if let Some((at, live)) = ui.data(|d| d.get_temp::<(u64, f64)>(live_key)) {
        if pass == at + 1 {
            *value = T::from_f64(live);
        }
    }
    let before = value.to_f64();
    let mut dv = egui::DragValue::new(value)
        .range(spec.hard.clone())
        .speed(spec.speed)
        .update_while_editing(false)
        .custom_parser(eval_number);
    if let Some(n) = spec.max_decimals {
        dv = dv.max_decimals(n);
    }
    if let Some(n) = spec.min_decimals {
        dv = dv.min_decimals(n);
    }
    if !spec.suffix.is_empty() {
        dv = dv.suffix(spec.suffix.clone());
    }
    let mut response = ui.add(dv);
    let id = response.id;
    let mut v = value.to_f64();

    // Esc の取り消し（上の説明）
    let esc_key = id.with("escaped");
    let pass = ui.ctx().cumulative_pass_nr();
    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(esc_key, (pass, v)));
    } else if let Some((at, kept)) = ui.data_mut(|d| d.remove_temp::<(u64, f64)>(esc_key)) {
        if pass == at + 1 {
            v = kept;
        }
    }

    // ドラッグ: 始めの値を覚える。やわらかい上限と Ctrl の刻み
    let start_key = id.with("drag_start");
    if response.drag_started() {
        ui.data_mut(|d| d.insert_temp(start_key, before));
    }
    let drag_start: Option<f64> = ui.data(|d| d.get_temp(start_key));
    if response.dragged() {
        if let Some(soft) = &spec.soft {
            let s = drag_start.unwrap_or(before);
            v = v.clamp(soft.start().min(s), soft.end().max(s));
        }
        if let Some(step) = spec.snap.filter(|s| *s > 0.0) {
            if ui.input(|i| i.modifiers.command) {
                v = (v / step).round() * step;
            }
        }
    }

    // 右クリックで既定値に戻す
    let mut reset = false;
    if let Some(d) = spec.default {
        response.context_menu(|ui| {
            if ui.button("既定値に戻す").clicked() {
                v = d;
                reset = true;
                ui.close();
            }
        });
    }

    // Ctrl+ホイール（上の説明）。打っている間は欄の文字を優先して動かさない
    let mut wheeled = false;
    if !response.dragged() && !response.has_focus() {
        let range = spec.soft.clone().map(|s| s.start().min(v)..=s.end().max(v)).unwrap_or_else(|| spec.hard.clone());
        let fine = ui.input(|i| i.modifiers.shift);
        if let Some(n) = wheel_number(ui, &response, v, spec.wheel_amount(fine), range) {
            v = n;
            wheeled = true;
        }
    }

    v = v.clamp(*spec.hard.start(), *spec.hard.end());
    *value = T::from_f64(v);
    let v = value.to_f64();
    if response.dragged() {
        ui.data_mut(|d| d.insert_temp(live_key, (pass, v)));
    } else {
        ui.data_mut(|d| d.remove::<(u64, f64)>(live_key));
    }

    let mut committed = false;
    if response.drag_stopped() {
        committed = ui.data_mut(|d| d.remove_temp::<f64>(start_key)).is_some_and(|s| s != v) || v != before;
    } else if !response.dragged() && v != before {
        committed = true;
    }
    committed |= (reset || wheeled) && v != before;
    let live = v != before;
    if live {
        response.mark_changed();
    } else {
        response.flags.remove(egui::response::Flags::CHANGED);
    }
    NumberOutput { response, committed, live }
}

/// `Slider` の右の数値欄の Esc の取り消し（`number_field` と同じ回避）。Esc のフレームの値を覚えておき、次のフレームで戻す。
/// 戻したフレームは、欄を描く前と値が同じなら `changed()` を偽にする。`before` は欄を描く前の値
pub fn restore_after_escape<T>(ui: &egui::Ui, response: &mut egui::Response, value: &mut T, before: T)
where
    T: egui::emath::Numeric + Send + Sync,
{
    let key = response.id.with("escaped");
    let pass = ui.ctx().cumulative_pass_nr();
    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(key, (pass, value.to_f64())));
    } else if let Some((at, kept)) = ui.data_mut(|d| d.remove_temp::<(u64, f64)>(key)) {
        if pass == at + 1 {
            *value = T::from_f64(kept);
            if value.to_f64() == before.to_f64() {
                // `flags` は egui の doc(hidden) の公開フィールド。`changed()` を外す手段がこれしかない（0.36.2）
                response.flags.remove(egui::response::Flags::CHANGED);
            }
        }
    }
}

/// スライダー（対数の目盛りなど `number_field` に無いもの）。右の数値欄は、打っている途中の値を使わず、数式を受け、入力中の Esc で入力前の値に戻す。
/// `style` に `Slider` の設定（対数・桁・単位）を渡す
pub fn slider_field<T>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: RangeInclusive<T>,
    style: impl FnOnce(egui::Slider<'_>) -> egui::Slider<'_>,
) -> egui::Response
where
    T: egui::emath::Numeric + Send + Sync,
{
    let before = *value;
    let (lo, hi) = (range.start().to_f64(), range.end().to_f64());
    let mut response = ui.add(style(egui::Slider::new(value, range).update_while_editing(false).custom_parser(eval_number)));
    restore_after_escape(ui, &mut response, value, before);
    // Ctrl+ホイール: 幅の 100 分の 1（Shift で 1000 分の 1。整数は 1 より細かくしない）。対数の目盛りでも同じ量にする
    if !response.dragged() && !response.has_focus() {
        let fine = ui.input(|i| i.modifiers.shift);
        let mut step = (hi - lo).abs() / if fine { 1000.0 } else { 100.0 };
        if T::INTEGRAL {
            step = step.round().max(1.0);
        }
        if let Some(n) = wheel_number(ui, &response, value.to_f64(), step, lo..=hi) {
            *value = T::from_f64(n);
            response.mark_changed();
        }
    }
    response
}

/// 数値の Ctrl+ホイール（`number_field`・`slider_field` の中身。egui の `Slider` や自前の欄にも使う）。
/// `response` の上で Ctrl+ホイールを回すと、上へ 1 段で `step` 増やし、下へ 1 段で減らす。`range` の端で止める。変えたら新しい値
pub fn wheel_number(ui: &egui::Ui, response: &egui::Response, value: f64, step: f64, range: RangeInclusive<f64>) -> Option<f64> {
    if !response.hovered() || !(step > 0.0) {
        return None;
    }
    let steps = ui.input(|i| ctrl_wheel_steps(&i.events));
    if steps == 0 {
        return None;
    }
    // ctrl_wheel_steps は下へ回すと +1。数値は上へ回すと増やす
    let raw = value - steps as f64 * step;
    // 元が刻みの倍数なら倍数にそろえる（0.1 を 3 回足して 0.30000000000000004 にしない）。倍数でない値（3.3 に 1 ずつ）はそのまま足す
    let on_grid = ((value / step) - (value / step).round()).abs() < 1e-6;
    let raw = if on_grid { (raw / step).round() * step } else { raw };
    let n = raw.clamp(*range.start(), *range.end());
    (n != value).then_some(n)
}

/// 数を打つ 1 行の欄（文字で持つもの）の Ctrl+ホイール。欄の文字が数そのもの（`12`・`-0.5`。式は対象外）なら、
/// その最後の桁の 1 つ分（`0.5` なら 0.1、`12` なら 1。Shift で 1 桁下）だけ動かし、同じ桁数の文字に書き直す。打っている間は動かさない。
/// `range` の端で止める。変えたら true（呼ぶ側は、打って確定したときと同じに扱う）
pub fn wheel_number_text(ui: &egui::Ui, response: &egui::Response, text: &mut String, range: RangeInclusive<f64>) -> bool {
    if response.has_focus() {
        return false;
    }
    let t = text.trim();
    let Ok(v) = t.parse::<f64>() else { return false };
    if !v.is_finite() || t.contains(['e', 'E']) {
        return false;
    }
    let fine = ui.input(|i| i.modifiers.shift);
    let decimals = t.split_once('.').map(|(_, f)| f.len()).unwrap_or(0) + usize::from(fine);
    let step = 10f64.powi(-(decimals as i32));
    let Some(n) = wheel_number(ui, response, v, step, range) else { return false };
    *text = format!("{n:.decimals$}");
    true
}

// ---- 小さな窓を Esc で閉じる ----

/// 小さな窓（設定・確認など）を Esc で閉じる合図。入力中・一覧のキー操作中・ポップアップが開いている間の Esc は、そちらを取り消すだけにする。
///
/// egui はフレームの初めに Esc でフォーカスを外すので、「フォーカスが無かったか」は前のフレームの終わりに覚えた値で見る。
/// 窓を描いたあと毎フレーム `end_frame` を呼ぶ（`EscapeToClose` はリスト選択の Ctrl+ホイールの部品の後）

// ---- リスト選択の Ctrl+ホイール ----

/// このフレームの Ctrl（Mac は Command）を押したホイールの段数。下へ回すと +1（次の選択肢）、上へ回すと -1
pub fn ctrl_wheel_steps(events: &[egui::Event]) -> i64 {
    events
        .iter()
        .map(|e| match e {
            egui::Event::MouseWheel { delta, modifiers, .. } if modifiers.ctrl || modifiers.command => {
                // Shift も押すと横のホイールとして届く環境がある。縦が 0 なら横を見る
                let d = if delta.y != 0.0 { delta.y } else { delta.x };
                if d < 0.0 {
                    1
                } else if d > 0.0 {
                    -1
                } else {
                    0
                }
            }
            _ => 0,
        })
        .sum()
}

/// リスト選択（`ComboBox`）の上で Ctrl+ホイールを回すと、前後の選択肢に切り替える（ユーザーの指示、2026-10-23。
/// 自作プラグインのリスト選択はすべてこれを通す）。`response` は `ComboBox::show_ui(..).response`。
/// 端では止まる（回り込まない）。切り替えたら true（呼ぶ側は、選び直したときと同じに扱う）。今の値が選択肢に無ければ何もしない
pub fn wheel_select<T: PartialEq + Clone>(ui: &egui::Ui, response: &egui::Response, current: &mut T, options: &[T]) -> bool {
    if !response.hovered() || options.is_empty() {
        return false;
    }
    let steps = ui.input(|i| ctrl_wheel_steps(&i.events));
    if steps == 0 {
        return false;
    }
    let Some(pos) = options.iter().position(|o| o == current) else { return false };
    let next = (pos as i64 + steps).clamp(0, options.len() as i64 - 1) as usize;
    if next == pos {
        return false;
    }
    *current = options[next].clone();
    true
}

/// 添字で持つリスト選択の Ctrl+ホイール（`wheel_select` の添字版）。切り替えたら true
pub fn wheel_select_index(ui: &egui::Ui, response: &egui::Response, index: &mut usize, len: usize) -> bool {
    if len == 0 {
        return false;
    }
    let options: Vec<usize> = (0..len).collect();
    let mut cur = (*index).min(len - 1);
    let changed = wheel_select(ui, response, &mut cur, &options);
    if changed {
        *index = cur;
    }
    changed
}

/// 小さな窓を Esc で閉じる合図（説明は上の「小さな窓を Esc で閉じる」）
#[derive(Default)]
pub struct EscapeToClose {
    busy_last_frame: bool,
}

impl EscapeToClose {
    /// このフレームの Esc が窓を閉じる合図か
    pub fn pressed(&self, ctx: &egui::Context) -> bool {
        !self.busy_last_frame && !ctx.any_popup_open() && ctx.input(|i| i.key_pressed(egui::Key::Escape))
    }
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        self.busy_last_frame = ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open();
    }
}

/// テストの書き方: フォーカスは Tab のキーで移す（フレームの中でフォーカスが移る。クリックと同じ）。
/// フレームの外から `memory_mut(|m| m.request_focus(id))` で当てると `gained_focus` が立たず、`revert_on_escape` が入力前の文字を覚えない。
/// 1 フレームの出力は `textures_delta.clear()` してから捨てる（デバッグビルドで epaint の検査が止める）
#[cfg(test)]
mod tests {
    use super::*;

    fn preedit(t: &str) -> egui::Event {
        egui::Event::Ime(egui::ImeEvent::Preedit { text: t.into(), active_range_chars: None })
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn ime_composing() {
        assert!(ime_composing_after(&[preedit("あ")], false));
        assert!(ime_composing_after(&[], true));
        assert!(!ime_composing_after(&[egui::Event::Ime(egui::ImeEvent::Commit("雨".into()))], true));
        assert!(!ime_composing_after(&[preedit("")], true));
    }

    #[test]
    fn ime_enter_is_dropped_only_with_ime() {
        let mut ev = vec![egui::Event::Ime(egui::ImeEvent::Commit("雨".into())), key(egui::Key::Enter), egui::Event::Text("\n".into())];
        assert!(drop_ime_enter_events(&mut ev));
        assert_eq!(ev.len(), 1);
        let mut ev = vec![key(egui::Key::Enter)];
        assert!(!drop_ime_enter_events(&mut ev));
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_for_search("ＡＢＣ　ｘｙ１"), "abc xy1");
        assert_eq!(normalize_for_search("フチドリ"), "ふちどり");
        assert_eq!(normalize_for_search("ﾌﾁﾄﾞﾘ"), "ふちどり");
        assert_eq!(normalize_for_search("ﾊﾟﾚｯﾄ"), "ぱれっと");
        assert_eq!(normalize_for_search("ｳﾞｨ"), "ゔぃ");
        assert_eq!(normalize_for_search("Glow_H"), "glow_h");
        assert_eq!(normalize_for_search("縁取り"), "縁取り");
    }

    #[test]
    fn score_order() {
        let n = normalize_for_search;
        let s = |h: &str, q: &str| match_score(&n(h), &n(q));
        assert_eq!(s("グロー", "ぐろ"), Some(3));
        assert_eq!(s("ソフト グロー", "ぐろ"), Some(2));
        assert_eq!(s("アフターグロー", "ぐろ"), Some(1));
        assert_eq!(s("グラデーション ロー", "ぐろ"), Some(0));
        assert_eq!(s("ぼかし", "ぐろ"), None);
        assert_eq!(s("Color Shift", "col sh"), Some(5));
        assert_eq!(s("Color Shift", "col zz"), None);
        assert_eq!(s("何でも", ""), Some(0));
        // 絞り込みでは離れた一致を出さない（ほかの点数は同じ）
        let f = |h: &str, q: &str| filter_score(&n(h), &n(q));
        assert_eq!(f("グラデーション ロー", "ぐろ"), None);
        assert_eq!(f("キャラ_001", "001"), Some(2));
        assert_eq!(f("キャラ_0101", "001"), None);
        assert_eq!(f("ソフト グロー", "ぐろ"), Some(2));
    }

    #[test]
    fn expressions() {
        assert_eq!(eval_number("3*2"), Some(6.0));
        assert_eq!(eval_number("10/5+4"), Some(6.0));
        assert_eq!(eval_number(" -(1.5 + 0.5) * 2 "), Some(-4.0));
        assert_eq!(eval_number("１２＋３"), Some(15.0));
        assert_eq!(eval_number("−3"), Some(-3.0));
        assert_eq!(eval_number("1e3"), Some(1000.0));
        assert_eq!(eval_number("1/0"), None);
        assert_eq!(eval_number("abc"), None);
        assert_eq!(eval_number("2*"), None);
        assert_eq!(eval_number(""), None);
    }

    fn run<R>(ctx: &egui::Context, events: Vec<egui::Event>, _modifiers: egui::Modifiers, mut f: impl FnMut(&mut egui::Ui) -> R) -> R {
        let mut out = None;
        let raw = egui::RawInput { events, ..Default::default() };
        let mut full = ctx.run_ui(raw, |ui| {
            out = Some(f(ui));
        });
        full.textures_delta.clear();
        out.unwrap()
    }

    #[test]
    fn number_field_reverts_on_escape_and_commits_on_enter() {
        let ctx = egui::Context::default();
        let spec = NumberSpec::new(0.0..=100.0, 1.0);
        let mut v = 10.0_f64;
        let id = egui::Id::new("n");
        let draw = |ui: &mut egui::Ui, v: &mut f64| ui.push_id(id, |ui| number_field(ui, v, &spec)).inner;
        run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        // フォーカスを当てて打つ
        let _rid = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v).response.id);
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![key(egui::Key::A), egui::Event::Text("3*2".into())], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        let o = run(&ctx, vec![key(egui::Key::Escape)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert!(!o.committed);
        for _ in 0..3 {
            let o = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
            assert_eq!(v, 10.0, "Esc の後も入力前の値のまま");
            assert!(!o.committed);
        }
        // 打って Enter → 式の値で確定する
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![egui::Event::Text("3*2".into())], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        let mut committed = false;
        committed |= run(&ctx, vec![key(egui::Key::Enter)], egui::Modifiers::NONE, |ui| draw(ui, &mut v)).committed;
        for _ in 0..3 {
            committed |= run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v)).committed;
        }
        assert_eq!(v, 6.0, "数式の値で確定する");
        assert!(committed);
    }

    #[test]
    fn slider_field_reverts_on_escape() {
        let ctx = egui::Context::default();
        let mut v = 1.0_f64;
        let id = egui::Id::new("sl");
        let draw = |ui: &mut egui::Ui, v: &mut f64| ui.push_id(id, |ui| slider_field(ui, v, 0.1..=10.0, |s| s.logarithmic(true))).inner;
        run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        // Tab で取っ手、もう一度 Tab で数値欄へ
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![egui::Event::Text("5".into())], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![key(egui::Key::Escape)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        for _ in 0..3 {
            run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
            assert_eq!(v, 1.0, "Esc の後も入力前の値のまま");
        }
    }

    #[test]
    fn text_field_reverts_on_escape() {
        let ctx = egui::Context::default();
        let mut t = String::from("10");
        let id = egui::Id::new("t");
        let draw = |ui: &mut egui::Ui, t: &mut String| ui.push_id(id, |ui| text_field(ui, t, |e| e)).inner;
        let _rid = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut t).response.id);
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        run(&ctx, vec![egui::Event::Text("5".into())], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        assert_eq!(t, "105");
        let o = run(&ctx, vec![key(egui::Key::Escape)], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        assert_eq!((o.reverted, o.committed, t.as_str()), (true, false, "10"));
        assert_eq!(t, "10");
    }

    #[test]
    fn search_field_waits_for_ime_commit() {
        let ctx = egui::Context::default();
        let mut t = String::new();
        let id = egui::Id::new("s");
        let draw = |ui: &mut egui::Ui, t: &mut String| ui.push_id(id, |ui| search_field(ui, t, |e| e)).inner;
        let _rid = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut t).response.id);
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        let o = run(&ctx, vec![preedit("あ")], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        assert_eq!(o.query, "", "変換中は絞らない");
        let mut ev = vec![egui::Event::Ime(egui::ImeEvent::Commit("雨".into())), key(egui::Key::Enter)];
        drop_ime_enter_events(&mut ev);
        let o = run(&ctx, ev, egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        assert_eq!(o.query, t);
        assert!(!o.submitted, "確定の Enter で実行しない");
    }

    fn wheel(dy: f32, ctrl: bool) -> egui::Event {
        let modifiers = if ctrl { egui::Modifiers::CTRL } else { egui::Modifiers::NONE };
        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, dy), phase: egui::TouchPhase::Move, modifiers }
    }

    #[test]
    fn ctrl_wheel_moves_the_combo_choice() {
        assert_eq!(ctrl_wheel_steps(&[wheel(-1.0, true), wheel(-1.0, true)]), 2, "下へ 2 段");
        assert_eq!(ctrl_wheel_steps(&[wheel(1.0, true)]), -1, "上へ 1 段");
        assert_eq!(ctrl_wheel_steps(&[wheel(-1.0, false)]), 0, "Ctrl なしは数えない");

        let ctx = egui::Context::default();
        let opts = ["a", "b", "c"];
        let mut cur = "a";
        let draw = |ui: &mut egui::Ui, cur: &mut &'static str| {
            let r = egui::ComboBox::from_id_salt("combo")
                .selected_text(*cur)
                .show_ui(ui, |ui| {
                    for o in opts {
                        ui.selectable_value(cur, o, o);
                    }
                })
                .response;
            (wheel_select(ui, &r, cur, &opts), r.rect)
        };
        let (_, rect) = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        run(&ctx, vec![egui::Event::PointerMoved(rect.center())], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        let (changed, _) = run(&ctx, vec![wheel(-1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        assert!(changed && cur == "b", "Ctrl+下で次: {cur}");
        run(&ctx, vec![wheel(-1.0, true), wheel(-1.0, true), wheel(-1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        assert_eq!(cur, "c", "端で止まる");
        let (changed, _) = run(&ctx, vec![wheel(-1.0, false)], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        assert!(!changed && cur == "c", "Ctrl なしのホイールでは変えない");
        // 上に乗っていなければ変えない
        run(&ctx, vec![egui::Event::PointerMoved(rect.right_bottom() + egui::vec2(200.0, 200.0))], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        let (changed, _) = run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut cur));
        assert!(!changed && cur == "c", "上に乗っていないときは変えない");
        // 添字版
        let mut i = 0usize;
        let draw_i = |ui: &mut egui::Ui, i: &mut usize| {
            let r = egui::ComboBox::from_id_salt("combo").selected_text(opts[*i]).show_ui(ui, |_| {}).response;
            wheel_select_index(ui, &r, i, opts.len())
        };
        run(&ctx, vec![egui::Event::PointerMoved(rect.center())], egui::Modifiers::NONE, |ui| draw_i(ui, &mut i));
        assert!(run(&ctx, vec![wheel(-1.0, true)], egui::Modifiers::NONE, |ui| draw_i(ui, &mut i)));
        assert_eq!(i, 1);
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn number_field_drags_even_when_the_caller_resets_the_value() {
        // 呼ぶ側は確定するまで書かないので、毎フレーム古い値（10）から欄を作る（EditHexus の設定パネルと同じ形）
        let ctx = egui::Context::default();
        let spec = NumberSpec::new(0.0..=100.0, 1.0);
        let stored = std::cell::Cell::new(10.0_f64);
        let id = egui::Id::new("d");
        let draw = |ui: &mut egui::Ui| {
            let mut v = stored.get();
            let o = ui.push_id(id, |ui| number_field(ui, &mut v, &spec)).inner;
            if o.committed {
                stored.set(v);
            }
            (v, o.committed, o.response.rect)
        };
        let (_, _, rect) = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui));
        let c = rect.center();
        run(&ctx, vec![egui::Event::PointerMoved(c)], egui::Modifiers::NONE, |ui| draw(ui));
        run(&ctx, vec![press(c, true)], egui::Modifiers::NONE, |ui| draw(ui));
        let mut last = 10.0;
        for i in 1..=10 {
            let (v, committed, _) = run(&ctx, vec![egui::Event::PointerMoved(c + egui::vec2(i as f32 * 2.0, 0.0))], egui::Modifiers::NONE, |ui| draw(ui));
            assert!(!committed, "ドラッグ中は確定しない");
            last = v;
        }
        assert!(last > 15.0, "ドラッグで値が動き続ける: {last}");
        let mut committed = false;
        committed |= run(&ctx, vec![press(c + egui::vec2(20.0, 0.0), false)], egui::Modifiers::NONE, |ui| draw(ui)).1;
        committed |= run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui)).1;
        assert!(committed, "離したら確定する");
        assert!(stored.get() > 15.0, "確定した値が残る: {}", stored.get());
    }

    #[test]
    fn number_field_ctrl_wheel_steps_and_stops_at_the_limit() {
        let ctx = egui::Context::default();
        let spec = NumberSpec::new(0.0..=12.0, 0.1).snap(5.0);
        let mut v = 3.0_f64;
        let id = egui::Id::new("w");
        let draw = |ui: &mut egui::Ui, v: &mut f64| {
            let o = ui.push_id(id, |ui| number_field(ui, v, &spec)).inner;
            (o.committed, o.response.rect)
        };
        let (_, rect) = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![egui::Event::PointerMoved(rect.center())], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        let (committed, _) = run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert!(committed && v == 8.0, "Ctrl+上で刻みの分だけ増え、すぐ確定する: {v}");
        run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert_eq!(v, 12.0, "上限で止まる");
        let (committed, _) = run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert!(!committed && v == 12.0, "上限からは動かない");
        run(&ctx, vec![wheel(-1.0, true), wheel(-1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert_eq!(v, 2.0, "Ctrl+下で減る");
        run(&ctx, vec![wheel(-1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert_eq!(v, 0.0, "下限で止まる");
        let (committed, _) = run(&ctx, vec![wheel(1.0, false)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert!(!committed && v == 0.0, "Ctrl なしでは変えない");
        run(&ctx, vec![egui::Event::PointerMoved(rect.right_bottom() + egui::vec2(200.0, 200.0))], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut v));
        assert_eq!(v, 0.0, "上に乗っていないときは変えない");
    }

    #[test]
    fn wheel_number_text_keeps_the_digits() {
        let ctx = egui::Context::default();
        let mut t = String::from("0.5");
        let draw = |ui: &mut egui::Ui, t: &mut String| {
            let r = ui.add(egui::TextEdit::singleline(t));
            (wheel_number_text(ui, &r, t, -1.0..=1.0), r.rect)
        };
        let (_, rect) = run(&ctx, vec![], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        run(&ctx, vec![egui::Event::PointerMoved(rect.center())], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        assert!(run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut t)).0);
        assert_eq!(t, "0.6");
        for _ in 0..6 {
            run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut t));
        }
        assert_eq!(t, "1.0", "上限で止まる");
        t = "a+1".into();
        assert!(!run(&ctx, vec![wheel(1.0, true)], egui::Modifiers::NONE, |ui| draw(ui, &mut t)).0, "式は動かさない");
    }

    #[test]
    fn escape_to_close_skips_when_busy() {
        let ctx = egui::Context::default();
        let mut esc = EscapeToClose::default();
        let mut t = String::new();
        let _rid = run(&ctx, vec![], egui::Modifiers::NONE, |ui| ui.text_edit_singleline(&mut t).id);
        esc.end_frame(&ctx);
        run(&ctx, vec![key(egui::Key::Tab)], egui::Modifiers::NONE, |ui| ui.text_edit_singleline(&mut t));
        esc.end_frame(&ctx);
        let closed = run(&ctx, vec![key(egui::Key::Escape)], egui::Modifiers::NONE, |ui| {
            ui.text_edit_singleline(&mut t);
            esc.pressed(ui.ctx())
        });
        esc.end_frame(&ctx);
        assert!(!closed, "入力中の Esc では閉じない");
        let closed = run(&ctx, vec![key(egui::Key::Escape)], egui::Modifiers::NONE, |ui| {
            ui.text_edit_singleline(&mut t);
            esc.pressed(ui.ctx())
        });
        assert!(closed, "どこにもフォーカスが無いときの Esc で閉じる");
    }
}
