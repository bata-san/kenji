# Kenji — 汎用リモートコンピューティング基盤

1台のWindows PCに導入するだけで、場所や用途を問わず計算資源を安全かつ自由に活用できる、小規模プライベートクラウドを構成するソフトウェアです。

- **汎用実行**: 任意のコマンド・スクリプト・常駐サービスを「ジョブ」として統一管理（開始/停止/再実行/再起動ポリシー/cronスケジュール/ログ収集）
- **リモートターミナル**: PowerShell（ConPTY）をブラウザから操作。タブ作成・自動再接続・見やすい配色・コピー＆ペースト対応で、画面移動・再読み込み後もセッションを継続
- **ファイル管理**: ユーザーごとの専用フォルダ内でアップロード/ダウンロード/リネーム/削除。管理者は「PC全体」モードで全ドライブを操作可能（重要領域は保護）
- **リソース監視**: CPU / メモリ / ディスク / GPU（NVIDIA）
- **遠隔アクセス**: Cloudflare Tunnel でポート開放・固定IP不要
- **常駐動作**: Windows Service として自動起動
- **セキュリティ**: SQLiteユーザー管理 + JWTログイン + APIキー + 3段階ロール（viewer / operator / admin）+ 監査ログ + ログインロックアウト

設計の詳細は [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)、API仕様は [docs/api.md](docs/api.md) を参照してください。

## クイックスタート（開発実行）

```powershell
# 1. ビルド
cd server
cargo build

# 2. 初回セットアップ（管理者アカウント作成）
target\debug\kenji.exe setup --username admin --password <パスワード>

# 3. 起動（フォアグラウンド）
target\debug\kenji.exe serve
# → http://127.0.0.1:8787 をブラウザで開く
```

Web UIは `web/` でビルドします（初回は `web/dist` にプレースホルダが入っています）:

```powershell
cd web
npm install
npm run build     # server 側を再起動すると dist が埋め込まれる
```

## 本番導入（Windows Service）

```powershell
# 管理者権限の PowerShell で
kenji.exe setup                                  # 対話形式で管理者アカウントを作成
kenji.exe service install                        # LocalSystem で登録（推奨: 専用アカウント）
# 権限を絞りたい場合（"サービスとしてログオン"権限のある標準ユーザー）:
# kenji.exe service install --account DOMAIN\svc-kenji --password <pw>

sc start kenji                                   # サービス開始
# 以後、再起動後も自動で起動
```

解除:

```powershell
sc stop kenji
kenji.exe service uninstall
```

## 外部アクセス（Cloudflare Tunnel）

1. [cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/) をインストール（PATH へ）
   - または `C:\ProgramData\kenji\bin\cloudflared.exe` に配置
2. Web UI の「設定」でトンネルを有効化（または `PUT /api/settings {"tunnel":{"enabled":true}}`）
3. `https://<ランダム>.trycloudflare.com` が発行され、同じURLで外部からアクセス可能

Quick Tunnel はアカウント不要ですが、恒常利用には Named Tunnel（独自ドメイン・Cloudflare Access によるZero Trust保護）を推奨します。

## CLIログインとターミナル操作

Kenjiの各ユーザーはターミナルから利用するCLIへ個別にログインします。利用したいユーザーでログインし、初回にそれぞれ実行してください:

```powershell
opencode auth login
claude   # 表示される手順でClaude Codeにログイン
omp      # Oh My Piの認証/セットアップ手順
```

ログイン状態は `data-dir/profiles/<ユーザーID>/` の各ツール専用領域に保存されます（ユーザーIDは内部用のハッシュです）。OpenCodeは `XDG_DATA_HOME` と `OPENCODE_CONFIG_DIR`、Claude Codeは `CLAUDE_CONFIG_DIR`、Oh My Piは `PI_CONFIG_DIR` で分離します。サーバー環境から継承した一般的なプロバイダーAPIキーはCLIプロセスに渡しません。

ターミナルでは文字をドラッグ選択して「コピー」、または `Ctrl+Shift+C` でコピーし、「貼り付け」または `Ctrl+Shift+V` で貼り付けできます。ブラウザのクリップボード許可が必要です（HTTPSまたはlocalhostで利用）。

## CLI

```
kenji serve          # フォアグラウンド実行（--bind --port --data-dir）
kenji setup          # 初回セットアップ（--username --password --data-dir --force）
kenji user add|list|passwd|delete   # ユーザー管理（--data-dir）
kenji service install|uninstall   # Windowsサービス管理（要管理者権限）
kenji service run    # SCMから呼ばれる本体
kenji mcp            # MCPサーバー（stdio）— AIエージェント接続用（--url --token）
```

APIは curl / PowerShell から直接利用可能です（`docs/api.md` 参照）:

```powershell
$login = Invoke-RestMethod -Method Post http://127.0.0.1:8787/api/auth/login `
  -ContentType "application/json" -Body (@{username="admin";password="..."} | ConvertTo-Json)
$H = @{ Authorization = "Bearer $($login.token)" }
Invoke-RestMethod http://127.0.0.1:8787/api/system/overview -Headers $H
```

## セキュリティ上の注意

- kenji はリモートからのコマンドを**サービスアカウントの権限で**実行します。昇格API（runas / UAC）は提供しません
- サービスアカウントには**標準ユーザーを推奨**します（LocalSystem は遠隔操作に強すぎます）
- すべての操作は監査ログに記録されます（設定画面から参照可）
- ファイル操作は既定で各ユーザーの専用フォルダ（`workspace/<ユーザー名>`）配下に限定されます。APIキーは `workspace/_shared` を共有します
- **管理者のみ** `scope=system` でPC全体（全ドライブ）の読み書きができます。ドライブ直下、`Windows`、`Program Files`、`ProgramData`、ユーザープロファイル直下などの削除・移動と、Kenji の機密ファイル（`auth.json` / `users.db*` / `config.json` / `jobs.json`）の削除・移動・上書きは拒否されます
- ターミナルとジョブの既定ディレクトリもユーザー専用フォルダです（ターミナル/ジョブ自体はサービスアカウント権限で動作するため、OSレベルのサンドボックスではありません）
- ターミナルから起動するOpenCode / Claude Code / Oh My Pi (OMP) はKenjiユーザーごとの設定ディレクトリを使用し、親プロセスから引き継いだ一般的なプロバイダーAPIキーは除去します。初回は各CLIの `auth login` 等を各ユーザーのターミナルで実行してください。プロファイルは `data-dir/profiles/` に保存されます
- **注意:** Kenjiユーザーのプロファイル分離は誤共有を防ぐもので、同一Windowsサービスアカウント内のOSセキュリティ境界ではありません。ターミナルで任意コマンドを実行できる利用者同士を相互に隔離するには、別Windowsアカウント／VM等が必要です
- 外部公開は Cloudflare Tunnel 経由のみ（localhost バインド + エッジTLS）。Cloudflare Access 併用を推奨

## スモークテスト

```powershell
cargo test                                  # 単体テスト（Rust: パス検証・cron・トンネルURL・MCP JSON-RPC）
powershell -File scripts/smoke.ps1           # API結合テスト（21項目: 認証/ジョブ/ファイル/キー/監査/設定）
powershell -File scripts/ws_smoke.ps1         # WebSocketターミナル結合テスト（ConPTYエコー検証）
powershell -File scripts/mcp_smoke.ps1       # → node scripts/mcp_smoke.mjs（MCP stdio結合テスト: 24項目）
node scripts/system_scope_smoke.mjs           # PC全体スコープ結合テスト（管理者の読み書き・保護パス・非管理者403）
node scripts/update_smoke.mjs                 # 自動アップデート結合テスト（フェイクGitHub API・再起動まで）
# その後 server を起動した状態で:
cd web && node ui_smoke.mjs                  # ヘッドレスUI結合テスト（jsdom: SPAが実APIデータを描画するか）
```

## レポジトリ構成

```
docs/        設計書とAPI仕様
server/      Rustバックエンド（axum / tokio / portable-pty / sysinfo / windows-service）
web/         Reactフロントエンド（Vite + Tailwind + xterm.js + recharts）
scripts/     スモークテスト
```

## ライセンス

MIT
