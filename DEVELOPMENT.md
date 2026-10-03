# ColorHistory_H

AviUtl2 向け色履歴プラグイン（Rust 製 `.aux2`）。`PaletteHistory.aux2`（teruyoshi 氏作、ソース非公開）の代替として作成。

- **バージョン:** 0.2.1（aviutl2-rs 0.47）
- **依存:** `aviutl2` / `aviutl2-eframe` **0.40**（ホスト AviUtl2 **2.1.1** 以上）
- ユーザー向け: [`自作スクリプトマニュアル/ColorHistory_H_Manual.md`](../../../自作スクリプトマニュアル/ColorHistory_H_Manual.md)

## ビルド

```powershell
cd AI\plugins\ColorHistory_H
.\build.ps1            # release → Plugin\ColorHistory_H\ColorHistory_H.aux2（AviUtl2 起動中は配置しない）
.\build.ps1 -NoDeploy
cargo test
```

## 構成

| ファイル | 役割 |
|---|---|
| `src/lib.rs` | プラグイン登録、共有状態 `AppState`、イベント（更新・フォーカス変更）を監視スレッドへ通知、編集メニュー |
| `src/watcher.rs` | 通知を受けて色を読み、記録先のリストへ記録。保存（変更から 1 秒まとめて書く）も担当 |
| `src/tracker.rs` | 「色を変えたときだけ記録する」判定（純粋ロジック、テストあり） |
| `src/edit_ops.rs` | 本体とのやり取り。読み取り `call_read_section`、書き込みはボタン操作のみ |
| `src/history.rs` | `Store`（リスト・記録先・設定）、上限、並び替え・検索、v1 形式の移行、保存と読み込み |
| `src/replace.rs` | 色の検索（しきい値・`<#rrggbb>`・保存済み `.aup2` の他シーン）と一括置換 |
| `src/eyedropper.rs` | 画面の色（`GetDC(NULL)` + `GetPixel`）と主ボタンの押下状態 |
| `src/color.rs` | `Rgb`、書式の解析とコピー形式 |
| `src/clipboard.rs` | Win32 直のクリップボード書き込み |
| `src/ui.rs` | egui のウィンドウ |

## 約束事（本体の Undo を壊さない）

`PaletteHistory` と `reset_frame_selection_on_focus` は、フォーカス変更に反応して `call_edit_section` を呼び、
本体が UI 操作中（マウス押下〜解放）の Undo を捨てる不具合を起こしていた
（`AI/host/issues/20260913_host_undo_last_operation_lost.md`、ルール `au2-rs-plugin`）。

- **イベントからは読み取り（`call_read_section`）だけ。** イベント処理のスレッドでは通知を送るだけで、読み取りも監視スレッドで行う
- **書き込み（`call_edit_section`）はボタン操作だけ**: 「選択色を適用」、右クリックの適用、「チェックした N 件を置換」。マウスは離れている
- 一括置換は 1 回の `call_edit_section` にまとめる（本体の Undo 1 回ぶん）。直前に値を読み直し、条件に合わなくなった項目は書かない
- 検索は読み取りだけ。`get_edit_info()` は読み取りセクションに入る前に呼ぶ（`ReadSection` は編集情報を持たない）
- 編集メニュー「色履歴: 選択中オブジェクトの色を記録」は本体の編集セクション内で呼ばれるので、通知だけ送る。「カーソル位置の色を記録」は本体 API を呼ばない
- ウィンドウで処理したキー（Ctrl+C、スポイト中の Esc）は `consume_key` で消費する（aviutl2-eframe が本体へ転送しないように）

## リスト

- `Store.lists` の先頭は常に `default`（名前「デフォルト」、改名・削除不可）
- `Store.active` が記録先。記録はそのリストにだけ入る
- v0.1.0 のファイル（トップレベル `entries`）は読み込み時に `default` へ移す。書き出しには残さない
- 件数の上限はリストごと

## 自動記録の判定

- フォーカス中オブジェクトの色項目（`get_effect_items` の `EffectItemType::Color`）をエイリアスから読む
- **同じオブジェクトのまま値が変わった項目だけ**を保留にする。選び直したときは基準を取り直すだけ
- 保留は **値が 500ms 変わらず、マウスボタンが離れてから**確定（色設定ウィンドウのドラッグ中の途中色を入れない）
- 透明色（空値）と、新しく現れた項目（エフェクト追加）は記録しない

## スポイト

- ボタンは `Sense::drag()` を足して、ドラッグ開始でスポイト状態に入る。以後の押下状態は `GetAsyncKeyState`（`SM_SWAPBUTTON` を考慮）で見る
- winit がボタン押下時に `SetCapture` するので、ウィンドウ外で離しても離す通知は本体へ行かない想定（winit 0.30 `capture_mouse`。実機未確認）
- 読むのは表示されている色。設定値とずれうる

## 一括置換

- しきい値は RGB の各成分の差（0〜64）
- 表示中のシーン: `get_edit_info()` の `scene_id` / `layer_max` → 全レイヤーの `objects_in_layer` → エイリアス。項目の種類（`Color` / `Text`）は読み取りセクションの外で問い合わせる
- 他のシーン: `on_project_load` / `on_project_save` で覚えた `.aup2` を読む（`[scene.N] name`、`[N] layer/scene/frame`、`[N.M] effect.name`）。`ObjectHandle` を持たないので置換対象外
- `effect_index` は同名エフェクトの何番目か（`.aup2` 側も同じ数え方をする）

## 保存

`Plugin\ColorHistory_H\history.json`（`version: 2`）。一時ファイルに書いてから置き換える。読めないファイルは `history.broken-<秒>.json` に退避してから空で始める。

`oldScript\history.palette` の 39 色（`ffffff` の重複を除く）は 2026-09-13 に一度だけ `default` へ取り込んだ（`last_used` = ファイル更新時刻 − 番号、`last_source` = 「PaletteHistory から取り込み」）。取り込み機能はプラグインに持たせていない。

## 未確認（v0.2.0）

1. オブジェクト更新イベントが、色設定ウィンドウ・オブジェクト設定での色変更で来るか（自動記録の前提）
2. `get_effect_items` がスクリプトの `--color@` 項目を `Color`、テキストの本文を `Text` として返すか（`セクション@スクリプト` のまま渡し、駄目なら `@` より前で再試行している）
3. オブジェクト設定のカラーコード欄が `rrggbb` の貼り付けを受け付けるか
4. 適用・一括置換（`set_effect_item` → 読み返し）が実機で通るか。置換が本体の Undo 1 回で戻るか
5. スポイトをウィンドウ外で離したとき、本体へクリックが伝わらないか
6. `objects_in_layer` が `layer_max` までの全オブジェクトを返すか（表示範囲外のレイヤー・フレームを含むか）
7. このプラグインを入れた状態で、フォーカス切り替え＋ドラッグの本体 Undo が正常か（読み取りだけなので壊さない想定）
