# BLF Decoder

<img src="assets/icon-256.png" width="96" align="right" alt="BLF Decoder icon">

BLF(Vector Binary Logging Format)と DBC ファイルを入力し、CAN 信号をデコードして CSV / Parquet に出力するデスクトップアプリケーションです。Python 環境や CAN 解析ソフトを必要とせず、単一の実行ファイルで動作します。

## ダウンロード

[Releases](../../releases) ページから最新の実行ファイルを取得できます。

- `blf_decoder-vX.Y.Z-windows-x64.exe` — Windows 用(そのまま実行できます)
- `blf_decoder-vX.Y.Z-linux-x64` — Linux 用

リリースはタグ(`v*`)の push をトリガーに GitHub Actions が自動でビルド・公開します。

## 機能

- **入力**: BLF ファイル + DBC ファイル
- **デコード**
  - DBC の定義(BO_ / SG_)に従って CAN フレームを信号値へ変換
  - Intel(リトルエンディアン)/ Motorola(ビッグエンディアン)両対応
  - Factor / Offset / Signed / Unsigned を適用
  - マルチプレクサ信号(M / m\<N\>)、IEEE float/double 信号(SIG_VALTYPE_)対応
  - CAN / CAN FD フレーム(CAN_MESSAGE, CAN_MESSAGE2, CAN_FD_MESSAGE, CAN_FD_MESSAGE_64)対応
- **出力**: CSV または Parquet(SNAPPY 圧縮)
- **GUI**: BLF / DBC / 出力フォルダ選択、出力形式選択、変換ボタン、進捗バー、エラー表示
- **性能**: zlib コンテナを 1 個ずつ展開し行単位で書き出すストリーミング処理のため、数 GB の BLF もメモリをほぼ一定に保ったまま処理できます

## 出力データ形式

1 列目が `Timestamp`、2 列目以降が DBC で定義された Signal 名(例: `EngineSpeed`, `VehicleSpeed`)です。信号名が複数メッセージで重複する場合は `メッセージ名.信号名` の形式で区別されます。

**データ形状**(選択可):

| 形状 | 内容 |
|---|---|
| 等間隔サンプリング(デフォルト) | 指定間隔(デフォルト 100 ms)のグリッドに前値ホールド(サンプル&ホールド)で整列。すべての行に全信号の最新値が入った、そのまま分析に使えるテーブルになります |
| フレーム単位(生データ) | 1 行 = 1 CAN フレーム。そのフレームに含まれない信号は空欄(CSV)/ null(Parquet) |

**時刻列**(選択可):

| モード | 内容 |
|---|---|
| 先頭からの経過秒(デフォルト) | `0.0, 0.1, 0.2, ...` のような相対秒 |
| UNIX エポック秒 | BLF ヘッダの計測開始時刻 + フレームのオフセット |

```
Timestamp,EngineSpeed,EngineTemp,VehicleSpeed   ← 等間隔サンプリング(100 ms)の例
0.0,800.0,10.0,
0.1,1135.5,11.0,40.5
0.2,1389.0,12.0,41.5
```

## 使い方

### GUI

実行ファイルをダブルクリック(または引数なしで起動)すると GUI が開きます。ジョブキュー方式で、複数の BLF を登録して一括変換できます。

1. BLF をウィンドウへドラッグ&ドロップ(複数可・フォルダ可)
2. DBC を選択 — BLF と同じフォルダにあれば自動検出されます
3. 出力形式(CSV / Parquet)を選択
4. 「変換」を押す(キューを順番に処理)

- 左ペイン: キュー(状態表示、右クリックで削除 / 並べ替え / 出力を開く)
- 右ペイン: 選択中ジョブの設定(DBC・出力先・形式・オプション・信号選択)
- 下部: 進捗(ファイル / フレーム数 / 現在のメッセージ / 経過・残り時間)と折りたたみ式ログ
- テーマ: システム / ダーク / ライト。最近使った DBC・出力先や既定設定は自動保存されます

オプション: 相対時刻、CanId 列の出力、検索付きの信号選択、DBC にない CAN ID のスキップ(オフでエラー検出)、既存ファイルの上書き制御。

出力ファイル名は `<BLFファイル名>.csv` / `<BLFファイル名>.parquet` になります。

### CLI(開発・自動化用)

```
blf_decoder --blf input.blf --dbc database.dbc --out ./output
    [--format csv|parquet]       出力形式(デフォルト: csv)
    [--layout resample|raw]      データ形状(デフォルト: resample)
    [--interval-ms <n>]          サンプリング間隔(デフォルト: 100)
    [--timestamp relative|epoch] 時刻列(デフォルト: relative)
```

## ビルド

[Rust](https://rustup.rs/) 1.85 以降(edition 2024)が必要です。

```
cargo build --release
```

生成物は `target/release/blf_decoder`(Windows では `blf_decoder.exe`)の単一実行ファイルです。Windows 用リリースビルドはコンソールを表示しません。

```
cargo test        # ユニット + 統合テスト
```

アイコンは `scripts/gen_icon.py`(要 Pillow)で生成し、`build.rs`(winresource)で exe に埋め込んでいます。

## モジュール構成

```
GUI (src/gui.rs) / CLI (src/main.rs)
 │
 ▼
BLF Reader (src/blf.rs)          … LOGG ヘッダ / LOG_CONTAINER(zlib) / CAN フレーム抽出
 │
 ▼
DBC Parser (src/dbc.rs)          … BO_ / SG_ / SIG_VALTYPE_ の解析
 │
 ▼
CAN Signal Decoder (src/decode.rs) … ビット抽出 + 物理値変換、列レイアウト決定
 │
 ▼
Row Shaper (src/shape.rs)          … 等間隔リサンプリング(前値ホールド)
 │
 ▼
Conversion Pipeline (src/convert.rs)
 │
 ├── CSV Exporter (src/export/csv.rs)
 └── Parquet Exporter (src/export/parquet.rs)
```

コア処理はライブラリ(`blf_decoder` クレート)として分離されており、GUI / CLI のどちらからも同じパイプラインを呼び出します。信号選択・時間範囲フィルタ・複数 BLF 一括変換などの将来拡張は `convert.rs` への追加で対応できる構成です。

## 制限事項

- BLF 内の CAN 以外のオブジェクト(LIN, FlexRay, イベント等)はスキップされます
- DBC の拡張マルチプレクサ定義(SG_MUL_VAL_)は単純な m\<N\> として扱われます
- 等間隔サンプリングはフレームが概ね時刻順で記録されていることを前提とします(通常のロガー出力は時刻順です)
