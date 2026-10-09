//! 本体のイベントを受けて色を読み、履歴に記録するスレッド
//!
//! - イベント（オブジェクト更新・フォーカス変更）は**通知だけ**受け取り、ここで読み取りを行う
//!   （イベント処理のスレッドでは本体の API を呼ばない）
//! - 読み取りは `call_read_section` のみ。連続したイベントは 1 回の読み取りにまとめる
//! - 保存も担当する（変更から 1 秒まとめて書く）

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::edit_ops;
use crate::history::{self, now_secs};
use crate::tracker::ChangeTracker;
use crate::SharedState;

pub enum Msg {
    /// オブジェクト更新・フォーカス変更
    Changed,
    /// 選択中オブジェクトの色を今すぐ記録する（メニュー・ボタン）
    RecordNow,
    Shutdown,
}

/// 読む間隔の下限（v0.2.2 で 60ms から広げた。値が落ち着いたとみなすのは 500ms なので、記録には響かない）
const MIN_READ_INTERVAL: Duration = Duration::from_millis(200);
const SAVE_DELAY: Duration = Duration::from_secs(1);

pub struct Watcher {
    tx: Sender<Msg>,
    handle: Option<JoinHandle<()>>,
}

impl Watcher {
    pub fn start(shared: SharedState) -> Self {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("ColorHistory_H watcher".into())
            .spawn(move || run(rx, shared))
            .ok();
        Self { tx, handle }
    }

    pub fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    pub fn notify_changed(&self) {
        let _ = self.tx.send(Msg::Changed);
    }

    pub fn record_now(&self) {
        let _ = self.tx.send(Msg::RecordNow);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn mouse_buttons_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
        .iter()
        .any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0)
}

fn run(rx: Receiver<Msg>, shared: SharedState) {
    let started = Instant::now();
    let mut tracker = ChangeTracker::default();
    let mut need_read = false;
    let mut record_now = false;
    let mut last_read: Option<Instant> = None;
    // 色の項目の住所の控え（同じオブジェクトの間は、エイリアスを読まずに色の項目だけを読む）
    let mut layout: Option<edit_ops::ColorLayout> = None;

    loop {
        let busy = need_read || tracker.has_pending() || shared.read().dirty_since.is_some();
        let timeout = if busy { Duration::from_millis(50) } else { Duration::from_millis(1000) };
        let mut msgs = Vec::new();
        match rx.recv_timeout(timeout) {
            Ok(msg) => msgs.push(msg),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => msgs.push(Msg::Shutdown),
        }
        // 連続したイベントは 1 回の読み取りにまとめる
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        let mut shutdown = false;
        for msg in msgs {
            match msg {
                Msg::Changed => need_read = true,
                Msg::RecordNow => {
                    need_read = true;
                    record_now = true;
                }
                Msg::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            save_if_dirty(&shared, true);
            break;
        }

        let now_ms = started.elapsed().as_millis() as u64;
        let read_due = last_read.is_none_or(|t| t.elapsed() >= MIN_READ_INTERVAL);
        if need_read && read_due {
            need_read = false;
            last_read = Some(Instant::now());
            match edit_ops::catch_panic(|| edit_ops::read_focused_colors(&mut layout)) {
                Ok(Some((id, items))) => {
                    tracker.observe(Some(id), &items, now_ms);
                    let mut s = shared.write();
                    let record_now_done = record_now;
                    if record_now {
                        record_now = false;
                        let mut count = 0;
                        for item in &items {
                            if let Some(color) = item.value {
                                s.store.record(color, &item.key.label(), now_secs());
                                count += 1;
                            }
                        }
                        s.status = if count > 0 {
                            format!("選択中オブジェクトの色を {count} 件記録しました")
                        } else {
                            "選択中オブジェクトに色の項目がありません".into()
                        };
                        if count > 0 {
                            s.mark_dirty();
                        }
                    }
                    // 値が変わったときだけ描き直す（ほかの項目のドラッグ中に、毎回描き直さない）
                    let changed = record_now_done || s.focused.as_ref() != Some(&items);
                    s.focused = Some(items);
                    if changed {
                        s.request_repaint();
                    }
                }
                Ok(None) => {
                    tracker.observe(None, &[], now_ms);
                    let mut s = shared.write();
                    let changed = record_now || s.focused.is_some();
                    if record_now {
                        record_now = false;
                        s.status = "オブジェクトが選択されていません".into();
                    }
                    s.focused = None;
                    if changed {
                        s.request_repaint();
                    }
                }
                Err(e) => {
                    // 起動直後・出力中などは読めない。黙って次の通知を待つ
                    tracing::debug!(error = %e, "read_focused_colors failed");
                    if record_now {
                        record_now = false;
                        shared.write().status = format!("色を読めませんでした: {e}");
                    }
                }
            }
        }

        let settled = tracker.take_settled(now_ms, mouse_buttons_down());
        if !settled.is_empty() {
            let mut s = shared.write();
            if s.store.settings.auto_record {
                // 選択中のリストにだけ入る
                for (color, source) in &settled {
                    s.store.record(*color, source, now_secs());
                }
                s.mark_dirty();
                s.request_repaint();
            }
        }

        save_if_dirty(&shared, false);
    }
}

fn save_if_dirty(shared: &SharedState, force: bool) {
    let (store, path) = {
        let s = shared.read();
        let Some(since) = s.dirty_since else {
            return;
        };
        if !force && since.elapsed() < SAVE_DELAY {
            return;
        }
        (s.store.clone(), s.path.clone())
    };
    match history::save(&path, &store) {
        Ok(()) => {
            shared.write().dirty_since = None;
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "ColorHistory_H: 履歴の保存に失敗");
            let mut s = shared.write();
            s.status = format!("履歴を保存できませんでした: {e}");
            // 失敗し続けてもログを埋めないよう、次の試行は時間を置く
            s.dirty_since = Some(Instant::now());
        }
    }
}
