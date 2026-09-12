# FUIDE Arch-Update — pacman / AUR のパッケージマネージャ + 更新監視トレイ (FUI)

Arch Linux (CachyOS など) 向けのデスクトップ用パッケージマネージャです。[fuide](https://github.com/kobago/fuide) (egui 0.36 向けの FUI テーマ・窓シェル・部品) の上に作ってあり、fuide-brew と同じ構成 (左: ビュー / システム、中央: 一覧、右: 詳細、下: イベントログ) です。

- **GUI `fuide-arch-update`** — インストール済み / 明示 / 更新 / AUR / 孤立 / 検索の 6 ビュー、パッケージの詳細、インストール・削除・全体更新・明示/依存の切替・孤立削除・キャッシュ掃除。root が要る操作は **polkit (`pkexec`) 経由で非対話に実行**します。パスワードは KDE / GNOME 標準の認証ダイアログが聞き、コマンドの出力は行ごとにイベントログへ流れます。端末もコンソールパネルもありません
- **トレイ `fuide-arch-update-tray`** — StatusNotifierItem。一定間隔で `checkupdates` + `yay -Qua` を実行し、アイコン (シアン = 最新 / アンバー + 点 = 更新あり / 赤 = チェック失敗) とメニュー (更新一覧、最終・次回チェック、Open / Upgrade now / Check) を更新。新しい更新が見つかると通知 (Open / Upgrade now アクション付き)。GUI とは状態ファイルを共有するので、GUI で更新するとトレイもすぐ「最新」になります

![ダッシュボード](screenshots/dashboard.png)

![全体更新: 確認ダイアログの後は polkit の認証、出力はイベントログへ](screenshots/upgrade.png)

## 構成

```
crates/archpkg/   pacman / AUR ヘルパーの読み取りクエリと出力パーサ (-Qi / -Si / -Ss / checkupdates / -Qua)、GUI とトレイが共有する状態ファイル
apps/gui/         fuide-arch-update  (egui + fuide。ストリーミングランナー、テスト用の偽 pacman / yay / pkexec)
apps/tray/        fuide-arch-update-tray  (ksni + tokio + notify-rust)
res/              .desktop 2 つ、アイコン、polkit のポリシー
pkg/PKGBUILD      fuide-arch-update-git
```

## 方針

- **システムを変える操作は全部 pacman / ヘルパーのコマンド**として実行する (libalpm には触らない)。GUI は読むだけ
- **非対話**: pacman は `--noconfirm`、ヘルパーは `--noconfirm --sudo pkexec`。質問は一切出ないので、AUR の PKGBUILD は事前に `PKGBUILD` ボタン (aur.archlinux.org) で確認してからインストールする
- **認証は polkit**: `pkexec pacman …` で KDE / GNOME の認証エージェントがパスワードを聞く。アプリはパスワードを見ない。`res/org.kobago.fuide-arch-update.policy` を `/usr/share/polkit-1/actions` に入れると (`make install PREFIX=/usr`) 認証が数分間保持され、全体更新 → 孤立削除と続けても 1 回で済む。無くても動く (毎回聞かれる)
- 部分アップグレード (`-Sy` + 個別 `-S`) は出さず、更新は常に `-Syu` (ヘルパーがあれば `yay -Syu`、無ければ `pkexec pacman -Syu`)。AUR パッケージ単体の更新はヘルパーの `-S <pkg>`
- 実行前に必ず確認ダイアログ (コマンド行を表示)。終了コード 0 で `SUCCESS`、それ以外は `ERROR` カード (polkit の 126 = 取消、127 = 拒否も区別)。終了後は在庫を再読込して再チェック → トレイに反映

## 画面

- **VIEWS** — INSTALLED / EXPLICIT / UPDATES / AUR / ORPHANS / SEARCH (Ctrl+1..6)。更新と孤立には注意色の点
- **SYSTEM** — 最新率ゲージ、パッケージ数 / 明示 / AUR / 更新、インストール容量、キャッシュ容量 (`paccache -dk2` の候補数)、孤立、DB 同期時刻、最終チェック、カーネル、AUR ヘルパー、権限昇格 (`pkexec`)、トレイ稼働。`CLEAN CACHE` (`paccache -rk2`)、`ORPHANS n` (`pacman -Rns`)
- **ツールバー** — 再読込 (Ctrl+R)、`CHECK` (Ctrl+U: `checkupdates` + `-Qua`、状態ファイルに書く → トレイに反映)、`UPGRADE ALL n`、フィルター (Ctrl+F)。SEARCH ビューでは検索欄 + `SEARCH` (`pacman -Ss` + `yay -Ss --aur`。同名はリポジトリ優先、インストール済みは在庫の情報をそのまま表示、未導入のものは選択時に `-Si` で詳細を取得)
- **一覧** — NAME / VERSION / LATEST / REPO / STATUS (OUTDATED / ORPHAN / EXPLICIT / DEP / AVAILABLE) / SIZE。列見出しでソート、ダブルクリックでホームページ
- **PACKAGE** — 説明、状態、版 / 最新、repo、容量、導入日、ビルド日、ライセンス、グループ、パッケージャ / メンテナ、AUR の票数 / 人気 / out-of-date、DEPENDS / REQUIRED BY / OPTIONAL / PROVIDES / CONFLICTS。`HOMEPAGE` / `COPY` / `PKGBUILD` (AUR) / `EXPLICIT` ↔ `AS DEP` (`pacman -D`)、`UPGRADE` (AUR) / `UPGRADE ALL` / `REMOVE` (`-Rns`、危険色) / `INSTALL` (`-S --needed` または `yay -S`)
- **EVENT LOG** — アプリ側の出来事とコマンドの出力 (`error:` は危険色、`warning:` は注意色、`::` / `==>` は正常色)。帯のドラッグで高さ変更、チップで折りたたみ (保存)。コマンドを開始すると自動で開く

| 操作 | キー |
|---|---|
| ビュー | Ctrl+1 … Ctrl+6 |
| 再読込 / チェック | Ctrl+R / Ctrl+U |
| フィルター (検索ビューでは検索欄) | Ctrl+F、Esc でクリア |
| 選択行のホームページ / 削除 | Enter / Ctrl+Backspace |
| ダイアログ | Enter = 実行、Esc = 取消 |
| 設定ウィンドウ | Ctrl+, または歯車 (パレット CYAN / AMBER / GREEN、角、密度、透過、エージェント) |
| 終了 | Ctrl+W (実行中のコマンドは走り切る) |

## トレイ

```
fuide-arch-update-tray [--interval SECS] [--no-initial-check]   # 既定 3600 秒、起動 20 秒後に初回チェック
```

- 左クリック = GUI を開く。メニュー: `N updates available` → GUI、Repositories / AUR のサブメニュー (項目をクリックすると GUI がそのパッケージを選択して開く `--select`)、`Last check` / `Next check`、`Open …`、`Upgrade now…` (GUI を `--upgrade` で開く = 全体更新の確認ダイアログから始まる)、`Check for updates`、`Quit`
- 通知: 前回のチェックに無かった更新が見つかったときだけ。`Open` / `Upgrade now` アクション
- 状態ファイル `~/.local/state/fuide-arch-update/check` を GUI と共有 (両方が書く。トレイは変更を監視して即反映)
- アイコンはコードで描く ARGB ピクスマップ (アイコンテーマ不要)。同じ図形が `res/fuide-arch-update.svg`
- 1 セッション 1 インスタンス (`$XDG_RUNTIME_DIR` のロック)
- GUI も 1 セッション 1 窓: 起動済みならトレイのクリックや 2 回目の起動は既存の窓を前面に出す (要求は `$XDG_RUNTIME_DIR/fuide-arch-update.sock` 経由。`--select` / `--upgrade` もそちらに渡る)

## ビルドと導入

### cargo install (root 不要)

```sh
sudo pacman -S --needed cargo pacman-contrib polkit    # checkupdates / paccache、pkexec (KDE / GNOME には認証エージェントが入っている)
export CARGO_NET_GIT_FETCH_WITH_CLI=true               # fuide は ssh の非公開リポジトリ: システムの git で取得 (~/.cargo/config.toml の [net] でも可)
cargo install --git https://github.com/kobago/fuide-arch-update.git fuide-arch-update fuide-arch-update-tray
fuide-arch-update --setup                              # スタートメニュー + アイコン + ログイン時のトレイ自動起動、トレイを今すぐ起動
```

手元の checkout から入れるなら、ルートは仮想マニフェストなので `--path .` ではなくクレートごとに指定します (この場合はリポジトリ内の `.cargo/config.toml` が効くので環境変数は不要):

```sh
cargo install --path apps/gui && cargo install --path apps/tray
fuide-arch-update --setup
```

`--setup` は `~/.local/share/applications` / `~/.local/share/icons` / `~/.config/autostart` に、自分のバイナリの絶対パスを書いた `.desktop` を置きます (`~/.cargo/bin` がセッションの PATH に無くても動く)。`fuide-arch-update --unsetup` で元に戻ります。polkit のポリシーだけはシステム側にしか置けないので、この形では root 操作のたびにパスワードを聞かれます。気になるなら 1 ファイルだけ入れます:

```sh
sudo install -Dm644 res/org.kobago.fuide-arch-update.policy /usr/share/polkit-1/actions/
```

### システムワイド (make / PKGBUILD)

```sh
make && sudo make install PREFIX=/usr                  # /usr/bin/fuide-arch-update{,-tray} + .desktop + アイコン + polkit ポリシー
fuide-arch-update --setup                              # 任意: ログイン時にトレイを自動起動 (ユーザーごと)
```

`pkg/PKGBUILD` (`fuide-arch-update-git`) もあります (`cd pkg && makepkg -si`)。Makefile はビルドではなく、バイナリ以外 (.desktop、アイコン、polkit ポリシー) を `DESTDIR` / `PREFIX` に置くための PKGBUILD 用の薄い層です。

`fuide` クレートは非公開リポジトリから ssh で取得します (`Cargo.toml` の git 依存 + `.cargo/config.toml` の `git-fetch-with-cli`)。手元の checkout を使うなら `~/.cargo/config.toml` に:

```toml
[patch."https://github.com/kobago/fuide.git"]
fuide = { path = "/home/you/projects/github.com/kobago/fuide/crates/fuide" }
```

## 開発

```sh
cargo run --release                            # GUI (実物の pacman / yay を読む。操作するまで何も変えない)
cargo run --release -p fuide-arch-update-tray -- --interval 300
cargo test                                     # パーサ / 状態ファイル / ストリーミング / アプリの状態機械 / E2E (egui_kittest)
cargo clippy --all-targets -- -D warnings

# 偽のツールチェーンで一通りの流れを見る (システムに触らない。pkexec も偽物で認証なし)
F=$PWD/apps/gui/fixtures; FUIDE_ARCH_PACMAN=$F/fake-pacman.sh FUIDE_ARCH_AUR_HELPER=$F/fake-yay.sh \
  FUIDE_ARCH_PKEXEC=$F/fake-pkexec.sh FUIDE_ARCH_CHECKUPDATES=$F/fake-checkupdates.sh \
  FUIDE_ARCH_STATE_DIR=/tmp/fau-state FAKE_SLOW=1 cargo run --release
```

| 環境変数 | 意味 |
|---|---|
| `FUIDE_ARCH_PACMAN` / `FUIDE_ARCH_AUR_HELPER` (`none` = 無し) / `FUIDE_ARCH_PKEXEC` (`none` = root 操作なし) / `FUIDE_ARCH_CHECKUPDATES` / `FUIDE_ARCH_PACCACHE` | 実行ファイルの差し替え |
| `FUIDE_ARCH_STATE_DIR` / `FUIDE_ARCH_SYNC_DIR` | 状態ディレクトリ / 同期 DB ディレクトリの差し替え |
| `FUIDE_CONFIG_DIR` | fuide 設定ファイルの置き場所 |
| `FUIDE_SCREENSHOT=/path/shot.tga` (`FUIDE_SCREENSHOT_FRAME=45`) | N フレーム後に撮影して終了 |
| `FUIDE_DEV_DIALOG=install\|remove\|upgrade\|error\|success`, `FUIDE_DEV_SEARCH=q`, `FUIDE_DEV_SETTINGS=1 FUIDE_DEV_EMBED=1` | 撮影用フック |
| `FAKE_UPDATES=n` / `FAKE_AUR_UPDATES=n` / `FAKE_FAIL=1` / `FAKE_AUTH_FAIL=dismiss\|deny` / `FAKE_SLOW=1` | 偽ツールチェーンのつまみ |

AI エージェント (MCP) から操作する: 設定ウィンドウで `MCP SERVER: ON` にして `fuide-arch-update --mcp` を MCP クライアントに登録 (fuide の README 参照)。既定では確認ダイアログの実行ボタンは人間に留保されます。
