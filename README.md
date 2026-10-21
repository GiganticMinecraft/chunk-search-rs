# chunk-search-rs

NBT でエンコードされたワールドデータから並列かつ高速にエンティティまたはタイルエンティティが含まれているチャンクを検知するためのコマンドラインユーティリティ

## Usage

```console
$ chunk-search-rs [--protobuf] [--threads <N>] <world folder>
```

* `--protobuf` を付けると [chunk-search.proto](https://github.com/GiganticMinecraft/gigantic-protocol/) の `SearchResult` を stdout に書く。付けなければ 1 行 1 チャンクで `(x, z)` を書く
* 結果はチャンク座標 (x, z) の昇順で、重複は含まない

## 対応しているワールド形式

Minecraft 1.12 〜 1.18.2 の Anvil 形式 (`r.<x>.<z>.mca`) に対応している。

| 読むファイル | 形式 | 見るタグ |
| --- | --- | --- |
| `<world>/region/r.*.mca` | DataVersion 2844 (21w43a) 以上 (1.18 以降) | ルート直下の `xPos` / `zPos` / `block_entities` / `entities` |
| `<world>/region/r.*.mca` | DataVersion 2844 未満、または DataVersion 無し (1.17 以前) | `Level` 配下の `xPos` / `zPos` / `TileEntities` / `Entities` (1.17 以降は `Entities` が無くてもよい) |
| `<world>/entities/r.*.mca` (1.17 以降、存在する場合のみ) | - | ルート直下の `Position` (`[I; x, z]`) / `Entities` |

* 同じ region ファイルの中に旧形式と新形式のチャンクが混在していてもよい (DataVersion をチャンクごとに見る)
* `region` と `entities` の結果は和集合を取る
* 読むディメンションは次の順に決める
  1. `<world>/region` に region ファイルがあればそれ (vanilla のワールドフォルダ内の `DIM-1` / `DIM1` は別ディメンションなので見ない)
  2. 無ければ region ファイルを含む `<world>/DIM-1/region` か `<world>/DIM1/region` (Bukkit のネザー / エンドのワールドフォルダ)。両方にあればエラー
  3. どこにも region ファイルが無ければ、存在する最初の `region` ディレクトリ (結果は空)
* `r.<x>.<z>.mca` の名前が正規形 (`r.01.0.mca` や `r.+1.0.mca` ではない) のファイルだけを読む。シンボリックリンクは辿る
* 圧縮形式は gzip (1)、zlib (2)、無圧縮 (3) に対応する。圧縮種別に 128 が足されたチャンク (1 MiB を超えて `c.<x>.<z>.mcc` に外部ファイル化されたチャンク) も読む
* region ファイルは読み取り専用で開く。1.20.5 以降の LZ4 (4) などには対応していない
* 致命的エラーが出た時点で残りのファイルの読み込みはやめる

## エラー時の挙動

| 状況 | 挙動 |
| --- | --- |
| チャンク単位の問題 (NBT が壊れている、解凍できない、必要なタグが無い、参照先の `c.*.mcc` が無い、location やチャンク長が範囲外、圧縮種別が存在しない値)、region ヘッダが 8 KiB 未満 (読めた分の location だけ使う。Minecraft と同じ) | stderr に `warning: ...` を出してそのチャンクを飛ばし、残りの結果を出す (終了コード 0) |
| `region` ディレクトリが無い、ディメンションを決められない、ファイルの読み込みに失敗した (I/O エラー、リンク切れの symlink を含む)、実在するが未対応の圧縮種別 (4 = LZ4、127 = カスタム) | stderr に `error: ...` を出し、**stdout には何も出さずに**終了コード 1 で終わる |

理由:

* 壊れたチャンクは Minecraft 自身も読めずに再生成するもので、そのチャンクのためにワールド全体の検索を失敗させる意味は薄い
* 一方、region ファイルごと読めない場合や未知の圧縮形式は、大量のチャンクを黙って取りこぼしているかもしれない。部分的な結果を完全な結果として呼び出し元に使わせないよう、出力せずに失敗させる。呼び出し元は終了コードを確認すること
* サイズ 0 の region ファイルは空として扱う (クラッシュ直後などに残ることがある)
* 警告・エラーはそれぞれ先頭 100 件だけ stderr に出し、残りは件数だけ出す。呼び出し元が stderr を読まない場合 (SeichiAssist の `Runtime.exec`) にパイプバッファが埋まって止まらないようにするため

## Build

1. レポジトリをクローン
2. `cargo build`

## Test

```console
$ cargo test
```

統合テスト (`tests/cli.rs`) は `tests/fixtures/worlds` のワールドに対してバイナリを実行する。fixture は `python3 tests/fixtures/gen.py tests/fixtures/worlds` で再生成できる (出力は決定的)。

## Dependencies

* [gigantic-protocol](https://github.com/GiganticMinecraft/gigantic-protocol/)

## Dependents

* [SeichiAssist](https://github.com/GiganticMinecraft/SeichiAssist/) ([tree](https://github.com/GiganticMinecraft/SeichiAssist/blob/43b29ebf716877eb0c84a5a8cd86368ed32c610b/src/main/resources/config.yml#L17))

## License

[MIT License](./LICENSE)
