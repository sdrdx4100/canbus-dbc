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

1 行が 1 CAN フレームに対応します。

| 列 | 内容 |
|---|---|
| `Timestamp` | UNIX エポック秒(BLF ヘッダの計測開始時刻 + フレームのオフセット) |
| 2 列目以降 | DBC で定義された Signal 名(例: `EngineSpeed`, `VehicleSpeed`)。そのフレームに含まれない信号は空欄(CSV)/ null(Parquet) |

信号名が複数メッセージで重複する場合は `メッセージ名.信号名` の形式で区別されます。

## 使い方

### GUI

実行ファイルをダブルクリック(または引数なしで起動)すると GUI が開きます。

1. BLF ファイルを選択
2. DBC ファイルを選択
3. 出力フォルダを選択
4. 出力形式(CSV / Parquet)を選択
5. 「変換」ボタンを押す

出力ファイル名は `<BLFファイル名>.csv` / `<BLFファイル名>.parquet` になります。

### CLI(開発・自動化用)

```
blf_decoder --blf input.blf --dbc database.dbc --out ./output --format csv
blf_decoder --blf input.blf --dbc database.dbc --out ./output --format parquet
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
Conversion Pipeline (src/convert.rs)
 │
 ├── CSV Exporter (src/export/csv.rs)
 └── Parquet Exporter (src/export/parquet.rs)
```

コア処理はライブラリ(`blf_decoder` クレート)として分離されており、GUI / CLI のどちらからも同じパイプラインを呼び出します。信号選択・時間範囲フィルタ・複数 BLF 一括変換などの将来拡張は `convert.rs` への追加で対応できる構成です。

## 制限事項(v0.1)

- BLF 内の CAN 以外のオブジェクト(LIN, FlexRay, イベント等)はスキップされます
- DBC の拡張マルチプレクサ定義(SG_MUL_VAL_)は単純な m\<N\> として扱われます
- 出力タイムスタンプは f64 のエポック秒です(サブマイクロ秒精度が必要な場合は将来拡張で対応予定)
