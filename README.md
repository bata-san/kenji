# Kenji — 汎用リモートコンピューティング基盤

1台のWindows PCに導入するだけで、場所や用途を問わず計算資源を安全かつ自由に活用できる、小規模プライベートクラウドを構成するソフトウェアです。

- **汎用実行**: 任意のコマンド・スクリプト・常駐サービスを「ジョブ」として統一管理（開始/停止/再実行/再起動ポリシー/cronスケジュール/ログ収集）
- **リモートターミナル**: PowerShell（ConPTY）をブラウザから操作
- **ファイル管理**: ワークスペース内のアップロード/ダウンロード/リネーム/削除
- **リソース監視**: CPU / メモリ / ディスク / GPU（NVIDIA）
- **遠隔アクセス**: Cloudflare Tunnel でポート開放・固定IP不要
- **常駐動作**: Windows Service として自動起動
- **セキュリティ**: JWTログイン + APIキー + 3段階ロール（viewer / operator / admin）+ 監査ログ + ログインロックアウト

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

## CLI

```
kenji serve          # フォアグラウンド実行（--bind --port --data-dir）
kenji setup          # 初回セットアップ（--username --password --data-dir --force）
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
- ファイル操作はワークスペースルート配下に限定されます
- 外部公開は Cloudflare Tunnel 経由のみ（localhost バインド + エッジTLS）。Cloudflare Access 併用を推奨

## スモークテスト

```powershell
cargo test                                  # 単体テスト（Rust: パス検証・cron・トンネルURL・MCP JSON-RPC）
powershell -File scripts/smoke.ps1           # API結合テスト（21項目: 認証/ジョブ/ファイル/キー/監査/設定）
powershell -File scripts/ws_smoke.ps1         # WebSocketターミナル結合テスト（ConPTYエコー検証）
powershell -File scripts/mcp_smoke.ps1       # → node scripts/mcp_smoke.mjs（MCP stdio結合テスト: 24項目）
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
