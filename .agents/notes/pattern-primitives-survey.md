# `hole-engine` パターン表現・機能サーベイ

`hole-engine`（`tomet` 非依存の純粋な regex 代替エンジン）に必要な表現力を洗い出すため、`tandr` 既存機能・glob・regex（主要方言）・Comby・Rosie（RPL / PEG）・命名規則/パス固有要素を整理したサーベイ。

---

## 1. 基本マッチ・文字クラス・Unicode

| 機能 / 概念 | regex（PCRE2 / Rust / RE2 / Onig / JS） | glob | `tandr`（現状） | Comby / Rosie | `hole-engine` での候補・メモ |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 任意1文字 | `.`（改行除く / `s` フラグで含む） | `?`（`/` 除く） | — | Rosie: `.`（1文字） | `any_char` / `char`（改行や `/` の扱いはオプションまたは別プリミティブ） |
| 任意文字列（ワイルドカード） | `.*`, `.+` | `*`（セグメント内）, `**`（再帰） | `<?>` / `<?(x)>` | Comby: `:[x]`（最短） | `<?>`（文脈・境界を意識したマッチ）、`<?many>` |
| 数字・英字・英数字 | `\d`, `[0-9]`, `[[:digit:]]`, `[[:alpha:]]`, `[[:alnum:]]` | `[0-9]`, `[a-z]` | — | Rosie: `[:digit:]`, `[:alpha:]` | `digit`, `alpha`, `alnum`, `hex`, `ascii` |
| 空白・非空白 | `\s`, `\S`, `\h`, `\v` | — | — | Comby: `:[x]` vs `:[x\n]` | `space`, `ws`（空白列）, `non_ws` |
| 単語文字 | `\w`, `\W` | — | — | — | `word`（ASCII / Unicode 切替可） |
| 文字集合・否定集合 | `[abc]`, `[^abc]`, `[a-z&&[^aeiou]]`（集合演算） | `[abc]`, `[!abc]` | — | Rosie: `[a-z]`, `[^a-z]` | `chars("abc")`, `not_chars("abc")`, `range('a', 'z')`（集合演算も検討） |
| Unicode カテゴリ / Script | `\p{L}`, `\p{N}`, `\p{Script=Hiragana}` | — | — | Rosie: `\p{...}` | `unicode(letter)` / `script(hiragana)` 等（重い場合は feature flag） |

---

## 2. 量指定子・繰り返し・マッチ戦略

| 機能 / 概念 | regex | glob | `tomet` `hole.tmt` 例 | Rosie (PEG) | `hole-engine` での候補・メモ |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 0回または1回（省略可能） | `?` | `?(pat)`（extglob） | `<?opt>[...]` | `pat?` | `opt` |
| 0回以上 | `*` | `*(pat)`（extglob） | `<?many>[...]` | `pat*` | `many`（`{min: 0}`） |
| 1回以上 | `+` | `+(pat)`（extglob） | — | `pat+` | `many1` / `many{min: 1}` |
| 回数指定 `{m}`, `{m,n}`, `{m,}` | `{n}`, `{m,n}`, `{m,}` | — | `{length: 5}` / `{min: 1, max: 5}` | `pat{m,n}` | `many{min: m, max: n}` / `repeat` |
| 区切り文字付き繰り返し | `(?:a(?:,a)*)?`（手書きが辛い） | — | — | — | **`sep_by(item, sep)`**（`a, b, c` 等。regex より圧倒的に書きやすい） |
| 最長一致 vs 最短一致 | `.*` vs `.*?` | 最長（通常） | `hole.tmt` でホスト側に委任 | PEG は基本欲張り＋順序選択 | 明示オプション（例: `{greedy: false}` / デフォルト方針の統一） |
| 所有量指定子 / カット | `*+`, `++`, `(?>...)`（PCRE/Java） | — | — | PEG: `>`（cut） | バックトラック抑制・決定性確保のためのオプション検討 |

---

## 3. 分岐・グループ・構造・先読み/後読み

| 機能 / 概念 | regex | glob | `tandr` / `hole.tmt` | Comby / Rosie | `hole-engine` での候補・メモ |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 選択（OR） | `a\|b` | `{a,b}`, `@(a\|b)` | `<?alt(...)>` | Rosie: `a / b`（順序付き） | `alt`（PEG 的な順序選択にするか、最長選択にするかは設計要決定） |
| 否定（これに一致しない） | `(?!pat).*` | `!pat`, `!(a\|b)` | — | Rosie: `!pat` | `not`（否定先読み / 除外） |
| キャプチャ（名前付き束縛） | `(?<name>...)` | — | `<?(name)>`, `<?int(name)>` | Comby: `:[name]` / Rosie: ルール名がそのまま AST 名 | 全マッチャが第1引数で名前束縛可能（`hole.tmt` 準拠） |
| バランスした括弧・構造 | 再帰 regex `(?R)`（一部方言のみ・難解） | — | — | **Comby**: `:[x]` が `()`, `[]`, `{}`, `""` のネストを壊さない | **`balanced` / `delimited("(", ")")`**（Comby の最大の強み。regex 代替として必須級） |
| 先読み・後読み | `(?=...)`, `(?!...)`, `(?<=...)`, `(?<!...)`（後読みは固定長制限が多い） | — | — | Rosie: `&pat`, `!pat`（先読み） | `followed_by` / `not_followed_by` / `preceded_by` / `not_preceded_by` |
| 後方参照（同じ値の一致） | `\1`, `\k<name>`（バックトラック爆発源） | — | `hole.tmt` でホスト側課題として言及 | — | 同一束縛名の再出現＝等値制約（`<?(x)>-<?(x)>`）にするか、明示 `ref(x)` にするか |

---

## 4. アンカー・境界・パス固有プリミティブ

| 機能 / 概念 | regex | glob | `fs-catasto`（`zone`）要件 | `hole-engine` での候補・メモ |
| :--- | :--- | :--- | :--- | :--- |
| 先頭・末尾アンカー | `^`, `$`, `\A`, `\z` | 暗黙に全体一致 | 全体一致 / 部分一致 | `start`, `end`, `full`（モードまたはアンカー） |
| 単語境界 | `\b`, `\B` | — | 単語検索（`tomet-search`） | `word_boundary` |
| 行頭・行末 / 改行 | `(?m)^`, `$`, `\R` | — | テキスト検索（`tandr`） | `line_start`, `line_end`, `newline` |
| **パスセグメント（`/` を跨がない）** | `[^/]+` | `*`（1階層） | `"crates/<?kebab>/src"` の `<?kebab>` | **重要**: `seg` または `{cross_sep: false}` 的な境界制御。`*` と `**` の違いを曖昧にせず表現 |
| **複数階層パス（`/` を跨ぐ）** | `.*` | `**` | `"tmtroot/<?tree>"` 等 | `tree` / `subpath` / `many`（階層数 `{min_depth, max_depth}` も表現可） |
| ファイル名・拡張子・幹 | 手書き `([^/]+)\.([^/.]+)$` | `*.rs` | `"<?kebab>.adr.tmt"` 等 | `stem`, `ext` 等の補助または構造表現 |

---

## 5. 型・命名規則・構造化フォーマット（`tandr` / `catasto` / 実用プリミティブ）

regex では毎回長大な式を書く羽目になるが、標準ライブラリとして持つと圧倒的に強い領域。

| カテゴリ | 項目 | `tandr`（現状） | 用途例 |
| :--- | :--- | :--- | :--- |
| **プリミティブ型**（`tomet-base-type`） | `string`, `int`, `uint`, `float`, `bool` | **あり**（`Type::from_name`） | `adr-<?int(n)>`、`min`/`max` オプション付き数値範囲 |
| **日付・時刻** | `date`, `datetime` | **あり**（`Type::from_name`） | `<?date(d)>-daily.tmt`（ISO 8601 等） |
| **ID・バージョン** | `version`（semver）, `uuid`, `nanoid`, `ulid` | `version`, `uuid`, `nanoid` **あり** | `rust@<?version(v)>.vocabulary.tmt` |
| **命名規則（Case）** | `kebab` (`kebab-case`), `snake` (`snake_case`), `camel` (`camelCase`), `pascal` (`PascalCase`), `screaming_snake` (`SCREAMING_SNAKE_CASE`), `lower`, `upper`, `title` | なし | `fs-catasto` の `zone`（ファイル名・ディレクトリ名規約）、`tandr` の識別子リネーム |
| **識別子・スラッグ** | `ident`, `slug` | なし | `adr-<?int(n)>-<?slug(title)>.tmt` |
| **リスト・グループ系** | `list`, `rest`, `element`, `declaration` | **あり**（`tandr` AST 用） | `tomet-hole` / `tandr` / `tomet-search` の構造側（`hole-engine` 自体は文字列・トークン列側を担当） |
| **よく使う定型**（Rosie 的標準ライブラリ） | `ipv4`, `ipv6`, `email`, `url`, `hex`, `sha256` | なし | ログ検索、URL/ハッシュ検証 |

---

## 6. Rosie / PEG・Comby から取り入れるべき設計上の視点

1. **名前付き定義による合成・再利用（Rosie RPL 的）**
   - 1本の巨大な文字列パターンに押し込まず、小さなパターンに名前を付けて合成・テストできる仕組み（例: `let slug = ...` や標準ライブラリ登録）。
2. **決定的なマッチ（線形時間 / ReDoS 回避）**
   - regex の破滅的バックトラック（ReDoS）を避けるため、PEG（順序付き選択 + メモ化 / 明示的バックトラック制御）または NFA/DFA ベースの設計を意識する。
3. **マッチ結果の構造化キャプチャ（ツリー）**
   - 従来の regex の「フラットなグループ番号 `1, 2, 3`」ではなく、ネストした名前付きキャプチャ（`hole.tmt` の `<?(id)>[adr-<?int(n)>-<?(slug)>]` のように親と子を両方取れる構造）。
4. **方言のない単一仕様**
   - PCRE / JS / POSIX / glob のような「環境によって `\b` や `*` や `.` の意味が変わる」曖昧さを排除し、空白・改行・Unicode・`/` 境界の扱いを明示的なプリミティブとオプションで固定する。
