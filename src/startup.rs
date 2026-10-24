// 生成物: 正本は AI/tools/plugin_common/startup.rs（開発用のリポジトリ）。ここを直さず、正本を直して
// `python AI/tools/sync_plugin_common.py` で写す（`--check` で同じかを確かめる）。
//! 起動の高速化: 自作プラグインの egui の窓を、本体が 1 本ずつ読み込む前に、別のスレッドで並べて作っておく。
//!
//! なぜ: 本体はプラグインを 1 本ずつ `new` → `register` の順に呼び、`register` で窓のハンドルを待つ。窓（winit の隠しウィンドウと
//! OpenGL の文脈）を作るのに 1 本あたり 0.2〜0.5 秒かかり、自作 26 本で約 10 秒になっていた（本体のログ `LoadLibrary` の間隔）。
//! 窓の作成は互いに独立なので、並べると 1 本あたり 1/3 ほどになる（8 並列で各 270〜410 ms を実測）。
//!
//! 仕組み:
//! - 窓の作成をプラグインの `new` より前に始める。ただし窓の中身（`app_creator`）はプラグインの状態を取るので、まだ無い。
//!   そこで先に作る窓の `app_creator` は「プラグインが `window()` で渡すまで待つ」箱（`Slot`）にする。
//! - 最初に読み込まれた自作プラグインが `kick_siblings()` で、同じ置き場の兄弟の DLL を読み込み、
//!   書き出し関数 `hb_prewarm`（`export_prewarm!`）を別スレッドで呼ぶ。`hb_prewarm` が先に窓を作って箱に入れる。
//! - 各プラグインは `EframeWindow::new` の代わりに `startup::window` を呼ぶ。先に作ってあればそれを取り、無ければ今まで通り作る。
//!
//! 止め方: 環境変数 `HB_NO_PREWARM` を付けるか、`Plugin` フォルダに `hb_no_prewarm.txt` を置く。止めると今まで通りの順に作る。
//! 本体が読まなかった（RejectModule など）プラグインの窓も先に作られ、箱が渡されないまま 2 分で諦める。DLL は外れない（参照を持つ）。

use aviutl2::{AnyResult, tracing};
use aviutl2_eframe::{AviUtl2EframeHandle, EframeWindow, eframe};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

/// 先読みの対象（窓を持つ自作プラグイン。`AI/tools/sync_plugin_common.py` の EGUI_PLUGINS と同じ）
const OWN: &[&str] = &[
    "BackupDiff_H", "ChapterExport_H", "ColorHistory_H", "ColorSpaces_H", "CurveEditor_H", "EditHexus", "EditScript_H", "FileFinder_H",
    "LayerSilenceCut_H", "ManuscriptPanel_H", "MediaCollect_H", "MediaProbe_H", "MidpointTable_H", "NodeEditor_H", "PaletteForge_H",
    "PhoneRemote_H", "TextReplace_H", "VideoScope_H",
];

/// 箱が渡されるのを待つ長さ
const WAIT: Duration = Duration::from_secs(120);

type AppResult = Result<Box<dyn eframe::App>, Box<dyn std::error::Error + Send + Sync>>;
type Creator = Box<dyn FnOnce(&eframe::CreationContext<'_>, AviUtl2EframeHandle) -> AppResult + Send>;

/// プラグインが渡す `app_creator` の入れ物
struct Slot {
    creator: Mutex<Option<Creator>>,
    ready: Condvar,
}

/// 先に作った窓（この DLL の中だけの表）
static PREWARMED: Mutex<Vec<(String, Arc<Slot>, EframeWindow)>> = Mutex::new(Vec::new());

fn disabled(plugin_root: Option<&std::path::Path>) -> bool {
    std::env::var_os("HB_NO_PREWARM").is_some() || plugin_root.is_some_and(|r| r.join("hb_no_prewarm.txt").exists())
}

/// 窓を作る。先に作ってあればその窓に `creator` を渡し、無ければ `EframeWindow::new` で作る。
pub fn window<F>(name: &str, creator: F) -> AnyResult<EframeWindow>
where
    F: 'static + Send + FnOnce(&eframe::CreationContext<'_>, AviUtl2EframeHandle) -> AppResult,
{
    let taken = {
        let mut list = PREWARMED.lock().unwrap_or_else(|e| e.into_inner());
        list.iter().position(|(n, _, _)| n == name).map(|i| list.remove(i))
    };
    match taken {
        Some((_, slot, win)) => {
            *slot.creator.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(creator));
            slot.ready.notify_all();
            tracing::info!("起動の先取り: 「{name}」の窓は先に作ってあった");
            Ok(win)
        }
        None => EframeWindow::new(name, creator),
    }
}

/// 窓を先に作る（`hb_prewarm` から別スレッドで呼ばれる）。`app_creator` は `window()` が渡すまで待つ。
pub fn prewarm(names: &[&str]) {
    for &name in names {
        let slot = Arc::new(Slot { creator: Mutex::new(None), ready: Condvar::new() });
        let waiting = slot.clone();
        let started = Instant::now();
        let (base_tx, base_rx) = mpsc::channel::<()>();
        let label = name.to_string();
        let made = EframeWindow::new(name, move |cc, handle| {
            // ここに来た＝窓と GL の文脈ができた（残りはプラグインの中身を作るだけ）
            let _ = base_tx.send(());
            tracing::info!("起動の先取り: 「{label}」の窓の土台ができた（始めてから {} ms）", started.elapsed().as_millis());
            let guard = waiting.creator.lock().unwrap_or_else(|e| e.into_inner());
            let (mut guard, _) = waiting
                .ready
                .wait_timeout_while(guard, WAIT, |c| c.is_none())
                .unwrap_or_else(|e| e.into_inner());
            match guard.take() {
                Some(creator) => {
                    let t = Instant::now();
                    let r = creator(cc, handle);
                    tracing::info!("起動の先取り: 「{label}」の中身を作った（{} ms）", t.elapsed().as_millis());
                    r
                }
                None => Err("プラグインが読み込まれなかったので、先に作った窓を使わなかった".into()),
            }
        });
        match made {
            Ok(win) => {
                PREWARMED.lock().unwrap_or_else(|e| e.into_inner()).push((name.to_string(), slot, win));
                // 土台ができるまで待つ（呼ぶ側が同時に作る本数を数えられるように）
                let _ = base_rx.recv_timeout(Duration::from_secs(30));
            }
            Err(e) => tracing::warn!("起動の先取り: 「{name}」の窓を先に作れなかった: {e:#}"),
        }
    }
}

/// この DLL が先読みを受けられるようにする（`hb_prewarm` を書き出す）。引数は、このプラグインが `window()` で作る窓の名前。
#[macro_export]
macro_rules! export_prewarm {
    ($($name:expr),+ $(,)?) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn hb_prewarm() {
            let _ = std::panic::catch_unwind(|| $crate::startup::prewarm(&[$($name),+]));
        }
    };
}

#[allow(non_snake_case)]
mod sys {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn LoadLibraryW(name: *const u16) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        pub fn GetModuleHandleExW(flags: u32, addr: *const c_void, out: *mut *mut c_void) -> i32;
        pub fn GetModuleFileNameW(module: *mut c_void, buf: *mut u16, size: u32) -> u32;
        pub fn CreateMutexW(attrs: *const c_void, owner: i32, name: *const u16) -> *mut c_void;
        pub fn GetLastError() -> u32;
    }
    pub const FROM_ADDRESS: u32 = 0x4;
    pub const UNCHANGED_REFCOUNT: u32 = 0x2;
    pub const ERROR_ALREADY_EXISTS: u32 = 183;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// この DLL のファイルの場所
fn own_path() -> Option<PathBuf> {
    unsafe {
        let mut module = std::ptr::null_mut();
        let ok = sys::GetModuleHandleExW(sys::FROM_ADDRESS | sys::UNCHANGED_REFCOUNT, own_path as *const std::ffi::c_void, &mut module);
        if ok == 0 {
            return None;
        }
        let mut buf = [0u16; 1024];
        let n = sys::GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 || n >= buf.len() {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}

/// 兄弟の自作プラグインを読み込んで、窓を先に作らせる。プロセスの中で最初に呼んだ DLL だけが行う。
/// `new` の中で（ログの準備の後に）呼ぶ。
pub fn kick_siblings() {
    let Some(me) = own_path() else { return };
    // Plugin\<名前>\<名前>.aux2 → Plugin
    let root = me.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    if disabled(root.as_deref()) {
        return;
    }
    let Some(root) = root else { return };
    let my_stem = me.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // 別の DLL がすでに始めていたら何もしない（この表は DLL ごとなので、名前の付いた Mutex で見張る）
    let first = unsafe {
        let name = wide("Local\\HexBrowns_prewarm_v1");
        sys::CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        sys::GetLastError() != sys::ERROR_ALREADY_EXISTS
    };
    if !first {
        return;
    }
    let started = std::time::Instant::now();
    let mut targets = Vec::new();
    for &name in OWN {
        if name == my_stem {
            continue;
        }
        let path = root.join(name).join(format!("{name}.aux2"));
        if path.is_file() {
            targets.push((name, path));
        }
    }
    // 同時に作る本数の上限（既定 4。`Plugin\hb_prewarm_parallel.txt` に数を書くと変える。0 で上限なし）。
    // 実測（17 本）: 上限なし 窓の土台まで最大 8〜10 秒・自作の読み込み 11〜14 秒 / 上限 4 は 4.7 秒・6 秒
    let limit: usize = std::fs::read_to_string(root.join("hb_prewarm_parallel.txt")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(4);
    tracing::info!("起動の先取り: 兄弟 {} 本の窓を並べて先に作る（同時に作る本数の上限 {}）", targets.len(), if limit == 0 { "なし".to_string() } else { limit.to_string() });
    let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
    for (name, path) in targets {
        let gate = gate.clone();
        let spawned = std::thread::Builder::new().name(format!("prewarm-{name}")).spawn(move || unsafe {
            let t0 = Instant::now();
            if limit > 0 {
                let (count, cv) = &*gate;
                let mut n = count.lock().unwrap_or_else(|e| e.into_inner());
                while *n >= limit {
                    n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
                }
                *n += 1;
            }
            let waited = t0.elapsed();
            let wpath = wide(&path.to_string_lossy());
            let module = sys::LoadLibraryW(wpath.as_ptr());
            let loaded = t0.elapsed();
            if !module.is_null() {
                let f = sys::GetProcAddress(module, c"hb_prewarm".as_ptr() as *const u8);
                if !f.is_null() {
                    let f: extern "system" fn() = std::mem::transmute(f);
                    f();
                } // 先読みに対応していない版は何もしない
            }
            if limit > 0 {
                let (count, cv) = &*gate;
                *count.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
                cv.notify_one();
            }
            tracing::info!(
                "起動の先取り: {name}: 待ち {} ms・DLL を読む {} ms・窓の土台まで {} ms",
                waited.as_millis(),
                (loaded - waited).as_millis(),
                (t0.elapsed() - loaded).as_millis()
            );
        });
        if let Err(e) = spawned {
            tracing::warn!("起動の先取り: スレッドを起こせなかった: {e}");
        }
    }
    tracing::info!("起動の先取り: 始めるまで {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
}
