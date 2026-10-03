//! スポイト（画面上の色を拾う）
//!
//! 拾うのは**画面に表示されている色**。プレビューの縮小・アンチエイリアス・色の合成の結果なので、
//! オブジェクトに設定した値とは一致しないことがある。

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_SWAPBUTTON};

use crate::color::Rgb;

/// マウスカーソル位置の画面の色。
pub fn color_at_cursor() -> Option<(Rgb, (i32, i32))> {
    unsafe {
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok()?;
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let c = GetPixel(hdc, pt.x, pt.y);
        let _ = ReleaseDC(None, hdc);
        if c.0 == CLR_INVALID {
            return None;
        }
        // COLORREF は 0x00BBGGRR
        let v = c.0;
        Some((Rgb::new((v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8), (pt.x, pt.y)))
    }
}

/// 論理的な主ボタン（左右入れ替え設定を考慮）が押されているか。
/// `GetAsyncKeyState` は物理ボタンを見るので、入れ替えているときは右ボタンを見る。
pub fn primary_button_down() -> bool {
    unsafe {
        let vk = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
        GetAsyncKeyState(vk.0 as i32) < 0
    }
}
