mod clipboard;
mod color;
mod edit_ops;
mod eyedropper;
mod history;
mod replace;
mod tracker;
mod ui;
mod watcher;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use aviutl2::generic::__bridge::GenericSingleton;
use aviutl2::raw_window_handle::HasWindowHandle;
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
    pub egui_ctx: Option<egui::Context>,
}

impl AppState {
    pub fn mark_dirty(&mut self) {
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

#[aviutl2::plugin(GenericPlugin)]
pub struct ColorHistoryPlugin {
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
    window_created: AtomicBool,
    shared: SharedState,
    watcher: Option<Watcher>,
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
            *guard = Some(aviutl2_eframe::EframeWindow::new(WINDOW_NAME, move |cc, handle| {
                Ok(Box::new(ui::ColorHistoryApp::new(cc, handle, shared, sender)))
            })?);
        }
        self.window_created.store(true, Ordering::Release);
        Ok(())
    }

    fn open_window(&self) -> AnyResult<()> {
        self.ensure_window()?;
        let hwnd = {
            let guard = self.window.lock();
            let window = guard.as_ref().expect("window set after ensure_window");
            let handle = window.handle()?;
            let raw = handle.window_handle().map_err(|e| anyhow::anyhow!("window handle: {e}"))?;
            match raw.as_raw() {
                aviutl2::raw_window_handle::RawWindowHandle::Win32(h) => h.hwnd.get() as isize,
                _ => anyhow::bail!("Win32 以外のウィンドウは未対応"),
            }
        };
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_SHOW};
            let hwnd = HWND(hwnd as _);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }

    fn open_window_from_menu() {
        if let Err(e) = Self::with_instance(|plugin| plugin.open_window()) {
            tracing::error!("色履歴を開けませんでした: {e:#}");
        }
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
                    s.status = format!("カーソル位置の色 {} を記録しました", color.hex());
                }
                None => s.status = "カーソル位置の色を読めませんでした".into(),
            }
            s.request_repaint();
        });
    }
}

impl aviutl2::generic::GenericPlugin for ColorHistoryPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("ColorHistory_H v{} 初期化", env!("CARGO_PKG_VERSION"));
        let path = history::default_path();
        let (store, warning) = match history::load(&path) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ColorHistory_H: 履歴を読み込めませんでした: {e:#}");
                (Store::default(), Some(format!("履歴を読み込めませんでした: {e:#}")))
            }
        };
        if let Some(w) = &warning {
            tracing::warn!("{w}");
        }
        let shared = Arc::new(RwLock::new(AppState {
            status: warning.unwrap_or_default(),
            store,
            path,
            project_path: None,
            dirty_since: None,
            focused: None,
            egui_ctx: None,
        }));
        Ok(Self {
            window: Mutex::new(None),
            window_created: AtomicBool::new(false),
            shared,
            watcher: None,
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
        registry.register_edit_menu("色履歴を開く", Self::open_window_from_menu);
        registry.register_edit_menu("色履歴: 選択中オブジェクトの色を記録", Self::record_now_from_menu);
        registry.register_edit_menu("色履歴: カーソル位置の色を記録", Self::record_cursor_color_from_menu);
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        self.shared.write().project_path = project.get_path();
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        // 履歴はプロジェクトではなく専用ファイルに保存する。プロジェクト保存の機会にも書き出しておく
        let (store, path, dirty) = {
            let mut s = self.shared.write();
            if let Some(p) = project.get_path() {
                s.project_path = Some(p);
            }
            (s.store.clone(), s.path.clone(), s.dirty_since.is_some())
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
