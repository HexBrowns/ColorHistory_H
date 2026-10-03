//! クリップボードへの文字列の書き込み（Win32 直）
//!
//! eframe 側のクリップボード機能の有無に左右されないよう、`CF_UNICODETEXT` を直接置く。

use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

pub fn set_text(text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // 他のアプリが開いている瞬間があるので、少しだけ待って再試行する
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return Err("クリップボードを開けませんでした".into());
        }
        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard: {e}"))?;
            let hmem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| format!("GlobalAlloc: {e}"))?;
            let ptr = GlobalLock(hmem) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hmem));
                return Err("GlobalLock に失敗しました".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hmem);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hmem.0))) {
                // 成功したときは所有権がクリップボードへ移る。失敗したときだけ自分で解放する
                let _ = GlobalFree(Some(hmem));
                return Err(format!("SetClipboardData: {e}"));
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
