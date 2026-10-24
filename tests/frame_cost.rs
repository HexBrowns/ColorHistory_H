//! 色相順の並べ替えに掛かる時間の目安（履歴の多い場合: 1000〜10000 色）。
//! ウィンドウは並べ替えた結果を覚えておき、履歴・設定・検索語が変わったときだけ並べ直す（`ui.rs` の `refresh_view`）。
//! ここで測るのは、その並べ直し 1 回の時間。v0.3.1 の並べ方（比較のたびに HSV を計算する）も同じ色で測って並べる。
//! 一括置換の判定の時間は `replace.rs` の `perf_search_500_objects`。
//! 時間は機械で変わるので、既定では走らせない:
//!   cargo test --release --test frame_cost -- --ignored --nocapture

use std::time::Instant;

use color_history_h::color::Rgb;
use color_history_h::colorspace::HueSpace;
use color_history_h::history::{ColorList, Entry, SortMode, MAX_ENTRIES_MAX};

fn sample_list(n: usize) -> ColorList {
    let mut l = ColorList::new("x", "x", 0);
    let mut seed = 0x9e3779b9u32;
    let mut i = 0u64;
    while l.entries.len() < n {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        l.record(Rgb::new((seed >> 16) as u8, (seed >> 8) as u8, seed as u8), "テキスト / 文字色", i, MAX_ENTRIES_MAX);
        i += 1;
    }
    l
}

fn sort_hue_v031(items: &mut [&Entry]) {
    items.sort_by(|a, b| {
        let key = |e: &Entry| {
            let (h, s, v) = e.color.hsv();
            let achromatic = s < 0.12 || v < 0.08;
            (achromatic, if achromatic { -v } else { h }, -v)
        };
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1)).then(ka.2.total_cmp(&kb.2))
    });
}

fn time_ms(reps: usize, mut f: impl FnMut() -> usize) -> f64 {
    let mut sink = 0;
    let t = Instant::now();
    for _ in 0..reps {
        sink += f();
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / reps as f64;
    assert!(sink > 0);
    ms
}

#[test]
#[ignore]
fn sort_cost() {
    for n in [1000, 5000, 10_000] {
        let l = sample_list(n);
        let reps = 20;
        let hsv = time_ms(reps, || l.view(SortMode::Hue, HueSpace::Hsv, "").1.len());
        let oklch = time_ms(reps, || l.view(SortMode::Hue, HueSpace::Oklch, "").1.len());
        let recent = time_ms(reps, || l.view(SortMode::Recent, HueSpace::Hsv, "").1.len());
        let old = time_ms(reps, || {
            let mut v: Vec<&Entry> = l.entries.iter().collect();
            sort_hue_v031(&mut v);
            v.len()
        });
        println!(
            "[INFO] 色 {n}: 色相順 HSV {hsv:.3} ms / Oklch {oklch:.3} ms / 新しい順 {recent:.3} ms（v0.3.1 の色相順 {old:.3} ms。v0.3.1 は毎フレーム並べ直していた）"
        );
        assert!(oklch < 50.0, "1 回の並べ直しが重い: {oklch} ms");
    }
}
