// 生成物: 正本は AI/tools/plugin_common/hexbrowns_mark.rs（開発用のリポジトリ）。ここを直さず、正本を直して
// `python AI/tools/sync_plugin_common.py` で写す（`--check` で同じかを確かめる）。
//! HexBrowns 製のプラグインの印。SplitLayout_H の「自作プラグインの窓を 1 つの別窓にまとめる」が、窓を作った DLL がこれを書き出しているかで自作を見分ける
//! （ファイル名では見分けない）。中身は版の数だけで、呼んでも何もしない。

/// 印の版（今は 1）
// edition 2024 の拠点（FileFinder_H）もあるので、どちらの版でも通る書き方にする
#[unsafe(no_mangle)]
pub extern "C" fn hexbrowns_plugin_v1() -> u32 {
    1
}
