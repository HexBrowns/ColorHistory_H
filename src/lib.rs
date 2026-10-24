mod colors;
mod egui_input;
mod ui_theme;
mod clipboard;
pub mod color;
pub mod colorspace;
pub mod edit_ops;
mod eyedropper;
pub mod history;
pub mod replace;
pub mod tracker;
mod hexbrowns_mark;
mod startup;
mod host_fonts;
mod ui;
mod watcher;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use aviutl2::generic::__bridge::GenericSingleton;
use aviutl2::AnyResult;
use aviutl2_eframe::egui;
use parking_lot::{Mutex, RwLock};

use edit_ops::EDIT_HANDLE;
use history::{now_secs, Store};
use tracker::ColorItem;
use watcher::Watcher;

pub const WINDOW_NAME: &str = "色履歴";

/// UI・監視スレッド・プラグイン本体で共有する状態。
pub struct AppState {
    pub store: Store,
    pub path: PathBuf,
    /// 開いているプロジェクトの `.aup2`（一括置換で他のシーンを探すのに使う）。未保存なら `None`
    pub project_path: Option<PathBuf>,
    /// 未保存の変更が最初に入った時刻
    pub dirty_since: Option<Instant>,
    /// フォーカス中オブジェクトの色項目（適用先の候補）。未選択なら `None`
    pub focused: Option<Vec<ColorItem>>,
    pub status: String,
    /// 状態の行の種類（色分け）。`say` で文言と一緒に決める
    pub status_kind: ui_theme::StatusKind,
    pub egui_ctx: Option<egui::Context>,
    /// 起動時に履歴ファイルを読めず、退避もできなかった。この起動の間は保存しない
    /// （読めなかったファイルを、空から始めた履歴で上書きしない）
    pub save_blocked: bool,
    /// 履歴か設定を変えるたびに増える番号（ウィンドウが並べ替えた結果を覚えておき、変わったときだけ並べ直す）
    pub revision: u64,
}

impl AppState {
    /// 状態の行に出す文と種類を決める
    pub fn say(&mut self, kind: ui_theme::StatusKind, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_kind = kind;
    }

    pub fn mark_dirty(&mut self) {
        // 保存しない間も中身は変わるので、並べ直しの印は必ず進める
        self.revision = self.revision.wrapping_add(1);
        // 保存しない間は未保存の印も付けない（保存を試みず、監視スレッドも短い間隔で回らない）
        if self.save_blocked {
            return;
        }
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    pub fn request_repaint(&self) {
        if let Some(ctx) = &self.egui_ctx {
            ctx.request_repaint();
        }
    }
}

pub type SharedState = Arc<RwLock<AppState>>;

/// 本体が自分で書く写し（`Backup\AutoBackup_*.aup2`・終了時の `resume.aup2` / `restart.aup2`）か。
/// これらの保存でも `on_project_save` が来ることがあるので、開いているプロジェクトの名前と取り違えない（推論。本体 2.1.14 のログに保存の行がある）
fn is_host_copy(path: &std::path::Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let in_backup = path.parent().and_then(|d| d.file_name()).is_some_and(|d| d.eq_ignore_ascii_case("Backup"));
    in_backup || name == "resume.aup2" || name == "restart.aup2"
}

/// プロジェクトとして扱うパス。本体の写し（`is_host_copy`）なら、中の `[project]` の `file=` に書かれた元のプロジェクト（空なら None = 未保存）。
/// 本体は起動時に resume.aup2 を開くので、そのままだと前回のプロジェクトが「resume.aup2 という知らないプロジェクト」になる（2026-10-24 のログ）
fn real_project_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if !is_host_copy(path) {
        return Some(path.to_path_buf());
    }
    let text = std::fs::read_to_string(path).ok()?;
    project_file_of(&text).map(std::path::PathBuf::from)
}

/// `.aup2` の `[project]` の節の `file=` の値（空なら None）
fn project_file_of(text: &str) -> Option<String> {
    let mut in_project = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_project = line == "[project]";
            continue;
        }
        if in_project {
            if let Some(v) = line.strip_prefix("file=") {
                return Some(v.trim().to_string()).filter(|v| !v.is_empty());
            }
        }
    }
    None
}

fn init_logging() {
    use aviutl2::tracing::Level;
    use aviutl2::tracing_subscriber::filter::Targets;
    use aviutl2::tracing_subscriber::prelude::*;

    let level = if cfg!(debug_assertions) { Level::DEBUG } else { Level::INFO };
    // aviutl2-rs 0.40 は本体 2.1.9 の設定項目の種類 17〜19（数値グループ / 設定グループ / セパレーター）を知らず、
    // get_effect_items のたびに「Unknown effect item type」を WARN で出す（その項目は結果から落ちるだけ）。
    // 色の判定には関係しない項目なので、この module の WARN は本体のログに出さない
    let filter = Targets::new()
        .with_default(level)
        .with_target("aviutl2::generic::binding::edit_handle", Level::ERROR);
    let _ = aviutl2::tracing_subscriber::fmt()
        .with_max_level(level)
        .event_format(aviutl2::logger::AviUtl2Formatter)
        .with_writer(aviutl2::logger::AviUtl2LogWriter)
        .finish()
        .with(filter)
        .try_init();
}

/// 初期化にかかった時間をログに 1 行出す（強化デバッグ。仕様 `AI/specifications/20261012_CurveEditor_H_キーフレーム速度_spec.md` の 7.0）。
/// `register` の始めに作り、`register` を抜けるとき（途中の `return` も含む）に new と register の時間を出す
struct InitTimeLog {
    new_time: std::time::Duration,
    register_start: std::time::Instant,
}

impl InitTimeLog {
    fn start(new_time: std::time::Duration) -> Self {
        Self { new_time, register_start: std::time::Instant::now() }
    }
}

impl Drop for InitTimeLog {
    fn drop(&mut self) {
        let register = self.register_start.elapsed();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        aviutl2::tracing::info!(
            "ColorHistory_H: 初期化 {:.1} ms（new {:.1} ms・register {:.1} ms）",
            ms(self.new_time + register),
            ms(self.new_time),
            ms(register)
        );
    }
}

#[aviutl2::plugin(GenericPlugin)]
pub struct ColorHistoryPlugin {
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
    window_created: AtomicBool,
    shared: SharedState,
    watcher: Option<Watcher>,
    /// `new` にかかった時間（初期化の時間をログに出すため。`InitTimeLog`）
    init_new_time: std::time::Duration,
}

impl ColorHistoryPlugin {
    fn ensure_window(&self) -> AnyResult<()> {
        if self.window_created.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut guard = self.window.lock();
        if guard.is_none() {
            let shared = Arc::clone(&self.shared);
            let sender = self.watcher.as_ref().map(|w| w.sender());
            *guard = Some(startup::window(WINDOW_NAME, move |cc, handle| {
                Ok(Box::new(ui::ColorHistoryApp::new(cc, handle, shared, sender)))
            })?);
        }
        self.window_created.store(true, Ordering::Release);
        Ok(())
    }

    /// 編集メニュー（ショートカットを割り当てられる）。
    /// メニューの処理は本体の編集セクション内で呼ばれるので、ここでは通知だけ送り、読み取りは監視スレッドで行う。
    fn record_now_from_menu() {
        Self::with_instance(|plugin| {
            if let Some(w) = &plugin.watcher {
                w.record_now();
            }
        });
    }

    /// 編集メニュー（ショートカット向け）: マウスカーソル位置の画面の色を、選択中のリストへ記録する。
    /// 本体の API は呼ばない（画面の色を読むだけ）。
    fn record_cursor_color_from_menu() {
        Self::with_instance(|plugin| {
            let mut s = plugin.shared.write();
            match eyedropper::color_at_cursor() {
                Some((color, _)) => {
                    s.store.record(color, "スポイト", now_secs());
                    s.mark_dirty();
                    s.say(ui_theme::StatusKind::Ok, format!("カーソル位置の色 {} を記録しました", color.hex()));
                }
                None => s.say(
                    ui_theme::StatusKind::Error,
                    "カーソル位置の色を読めませんでした。画面に表示されている場所にカーソルを置いて、もう一度試してください",
                ),
            }
            s.request_repaint();
        });
    }
}

impl aviutl2::generic::GenericPlugin for ColorHistoryPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        let init_t0 = std::time::Instant::now();
        init_logging();
        startup::kick_siblings();
        tracing::info!("ColorHistory_H v{} 初期化", env!("CARGO_PKG_VERSION"));
        let path = history::default_path();
        let history::Loaded { store, warning, save_blocked } = history::load(&path);
        if let Some(w) = &warning {
            tracing::warn!("ColorHistory_H: {w}");
        }
        let shared = Arc::new(RwLock::new(AppState {
            status_kind: if warning.is_some() { ui_theme::StatusKind::Warn } else { ui_theme::StatusKind::Info },
            status: warning.unwrap_or_default(),
            store,
            path,
            project_path: None,
            dirty_since: None,
            focused: None,
            egui_ctx: None,
            save_blocked,
            revision: 0,
        }));
        Ok(Self {
            window: Mutex::new(None),
            window_created: AtomicBool::new(false),
            shared,
            watcher: None,
            init_new_time: init_t0.elapsed(),
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "ColorHistory_H".to_string(),
            information: format!(
                "ColorHistory_H v{} - 色履歴 / by HexBrowns",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        // 抜けるときに初期化の時間をログに出す（`InitTimeLog`）
        let _init_time = InitTimeLog::start(self.init_new_time);
        EDIT_HANDLE.init(registry.create_edit_handle());
        self.watcher = Some(Watcher::start(Arc::clone(&self.shared)));

        if let Err(e) = self.ensure_window() {
            tracing::error!("色履歴ウィンドウの初期化に失敗: {e:#}");
        } else {
            let guard = self.window.lock();
            if let Some(window) = guard.as_ref() {
                match window.handle() {
                    Ok(handle) => {
                        if let Err(e) = registry.register_window_client(WINDOW_NAME, &handle) {
                            tracing::error!("register_window_client 失敗: {e}");
                        }
                    }
                    Err(e) => tracing::error!("色履歴ウィンドウのハンドル取得に失敗: {e:#}"),
                }
            }
        }
        // ウィンドウは表示メニューから開く（メニューから ShowWindow しても開かない。2026-10-10 にユーザーの指摘で外した）
        registry.register_edit_menu("色履歴: 選択中オブジェクトの色を記録", Self::record_now_from_menu);
        registry.register_edit_menu("色履歴: カーソル位置の色を記録", Self::record_cursor_color_from_menu);
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        // 本体の写し（起動時の resume.aup2・バックアップ）は、中に書かれた元のプロジェクトとして扱う（書かれていなければ未保存）
        let path = project.get_path().and_then(|p| real_project_path(&p));
        let mut s = self.shared.write();
        // 記録先: このプロジェクトで最後に選んだリスト → 名前の合うリスト → デフォルト（history.rs の open_project）
        let o = s.store.open_project(path.as_deref());
        let name = s.store.active_list().name.clone();
        // 変わらなかったときも 1 行出す（切り替わらないと言われたとき、どう決めたかをログで追えるように）
        tracing::info!(
            "ColorHistory_H: プロジェクトを開いた（{}）。記録先は「{name}」（{}{}）",
            if path.is_some() { "保存済み" } else { "未保存" },
            o.reason.text(),
            if o.changed { "。切り替えた" } else { "。そのまま" }
        );
        if o.changed {
            s.say(ui_theme::StatusKind::Info, format!("記録先を「{name}」にしました（{}）", o.reason.text()));
            s.request_repaint();
        }
        if o.changed || o.reason == history::OpenReason::NameMatched {
            s.mark_dirty();
        }
        s.project_path = path;
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        // 履歴はプロジェクトではなく専用ファイルに保存する。プロジェクト保存の機会にも書き出しておく
        let (store, path, dirty) = {
            let mut s = self.shared.write();
            if let Some(p) = project.get_path().filter(|p| !is_host_copy(p)) {
                // 新規を初めて保存した・別名で保存したときは、今の記録先をその名前で覚える
                if s.store.project_saved(&p) {
                    s.mark_dirty();
                }
                s.project_path = Some(p);
            }
            (s.store.clone(), s.path.clone(), s.dirty_since.is_some() && !s.save_blocked)
        };
        if dirty {
            match history::save(&path, &store) {
                Ok(()) => self.shared.write().dirty_since = None,
                Err(e) => tracing::warn!("ColorHistory_H: 履歴の保存に失敗: {e:#}"),
            }
        }
    }

    fn event_update_object_info(&mut self) {
        if let Some(w) = &self.watcher {
            w.notify_changed();
        }
    }

    fn event_change_focus_object(&mut self) {
        if let Some(w) = &self.watcher {
            w.notify_changed();
        }
    }
}

aviutl2::register_generic_plugin!(ColorHistoryPlugin);

#[cfg(test)]
mod tests {
    use super::is_host_copy;
    use std::path::Path;

    #[test]
    fn host_copies_are_not_taken_as_the_project() {
        assert!(is_host_copy(Path::new(r"C:\ProgramData\aviutl2\Backup\AutoBackup_2026-10-24_07-18-37-446.aup2")));
        assert!(is_host_copy(Path::new(r"C:\ProgramData\aviutl2\resume.aup2")));
        assert!(!is_host_copy(Path::new(r"D:\Videos\作品A\作品A.aup2")));
    }
}

#[cfg(test)]
mod project_path_tests {
    use super::*;

    /// 本体の写しは中の元のプロジェクトとして扱う（resume.aup2 の file= が空なら未保存）
    #[test]
    fn host_copies_use_the_original_project() {
        assert_eq!(project_file_of("[project]\nversion=2011500\nfile=D:/a/b.aup2\n[scene.0]\nfile=x\n").as_deref(), Some("D:/a/b.aup2"));
        assert_eq!(project_file_of("[project]\nfile=\n"), None);
        assert_eq!(project_file_of("[scene.0]\nfile=x\n"), None, "[project] の外の file= は見ない");
        assert!(is_host_copy(std::path::Path::new(r"C:\ProgramData\aviutl2\resume.aup2")));
        let p = std::path::Path::new(r"D:\Videos\a.aup2");
        assert_eq!(real_project_path(p).as_deref(), Some(p), "写しでなければそのまま");
    }
}

// 本体が読み込む前に、兄弟のプラグインがこの窓を先に作れるようにする（startup.rs）
export_prewarm!(WINDOW_NAME);
