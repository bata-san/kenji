# Kenji — 汎用リモートコンピーティング基盤 アーキテクチャ設計

Windows PC を、インターネット経由で自由に使える小規模プライベートクラウドに変換するプラットフォーム。
特定用途（LLM推論・レンダリング等）に依存せず、**汎用的な「実行・管理・監視」**を中核に置く。

---

## 1. 実現可能性の検討（要件 → 実現手段）

| 要件 | 実現手段 | 成熟度/リスク |
|---|---|---|
| 簡単な導入 | 単一バイナリ `kenji.exe` + `kenji setup` / `kenji service install`。初回のみ管理者が `setup` とサービス登録を実施 | 低リスク |
| 常駐動作 | Windows Service（SCM 経由、自動起動）。`windows-service` クレート | 低リスク |
| 遠隔アクセス | Cloudflare Tunnel（ポート開放・固定IP不要、エッジでTLS終端）。`cloudflared` をサーバーが子プロセスとして管理 | 低リスク |
| 汎用実行環境 | ジョブモデル = 「コマンド + 引数 + cwd + env + 再起動ポリシー + cron」。任意のプロセス/常駐サービス/スクリプトを統一管理 | 低リスク |
| ターミナル | ConPTY（`portable-pty`）⇔ WebSocket ⇔ xterm.js。サービスアカウント権限で実行 | 低リスク |
| ジョブ管理 | 統一ジョブレジストリ（開始/停止/再実行/スケジュール/ログ収集/ローテーション） | 低リスク |
| リソース管理 | sysinfo（CPU/メモリ/ディスク）+ NVML（GPU）。プロセス停止・リソース制限は段階導入 | 中リスク（制限はPhase 2） |
| ファイル管理 | ワークスペース・サンドボックス化されたファイルAPI（アップロード/ダウンロード/削除/リネーム） | 低リスク |
| 外部サービス公開 | Cloudflare Tunnel に個別トンネルを追加（Phase 2）。MVPでは `cloudflared` をジョブとして起動すれば即可能 | 低リスク |
| 拡張性 | WSL2/Docker は「任意コマンド実行」で即日利用可能。マルチマシンは Phase 3 | 低リスク |
| 操作性 | 組み込みWeb UI（React）、REST API、WebSocket API。CLIは API を叩く薄いクライアントとして後段追加 | 低リスク |
| セキュリティ | JWTログイン + APIキー + 3段ロール + 監査ログ + ログインロックアウト + localhostデフォルトバインド | 低リスク |

**結論**: 全要件は既存の成熟したOSS（axum, tokio, portable-pty, sysinfo, cloudflared, xterm.js）とWindows標準機能（SCM, ConPTY, タスクキル）の組み合わせで実現可能。最大の設計リスクは複雑化であり、これを「1バイナリ・1設定ディレクトリ・1ジョブモデル」で抑える。

---

## 2. 全体アーキテクチャ

```
                    ┌───────────────────────────────┐
                    │        利用者（ブラウザ/CLI）   │
                    └──────────────┬────────────────┘
                                   │ HTTPS (Cloudflare Edge TLS)
                    ┌──────────────▼────────────────┐
                    │  Cloudflare Tunnel (cloudflared,│
                    │  アウトバウンド接続のみ・ポート開放不要)│
                    └──────────────┬────────────────┘
                                   │ http://127.0.0.1:8787
┌──────────────────────────────────▼───────────────────────────────────┐
│ kenji.exe（単一バイナリ：Windows Service として常駐）                   │
│                                                                      │
│  HTTP/WS API (axum)  ── 認証(RBAC) ── 監査ログ                       │
│   ├─ /api/auth/*      ログイン(JWT)・APIキー管理                      │
│   ├─ /api/jobs/*      ジョブ実行・管理・ライブログ(WS)                 │
│   ├─ /api/terminal    ConPTY ↔ WebSocket (PowerShell)                │
│   ├─ /api/files/*     ワークスペース内ファイル操作                    │
│   ├─ /api/system/*    CPU/GPU/メモリ/ディスク監視                     │
│   ├─ /api/settings, /api/audit, /api/update/*, /api/health             │
│  組み込みWeb UI (React 静的アセットをバイナリに埋め込み)               │
│                                                                      │
│  ジョブマネージャ ── 子プロセス(任意コマンド) ── ログ収集・ローテーション │
│  スケジューラ(cron)・再起動ポリシー・プロセスツリー強制停止            │
│  トンネルスーパーバイザ ── cloudflared 子プロセス管理                 │
└──────────────────────────────────────────────────────────────────────┘
        │
        ▼  データ: C:\ProgramData\kenji\
           config.json / auth.json / jobs.json / audit/ / logs/ / workspace/
```

- **外部公開は常に Cloudflare Tunnel 経由**で、ローカルポートはデフォルトで `127.0.0.1` にのみバインド。LANにも出さない。
- `cloudflared` 自体もジョブシステムと同じ「子プロセス管理」の仕組みで管理する（専用機能を作らない方針の体現）。

---

## 3. セキュリティモデル

### 認証
- **初回セットアップ（管理者・ローカル操作）**: `kenji setup` で管理者ユーザーとJWTシークレットを生成。`auth.json` には Argon2id ハッシュのみ保存し、`icacls` で SYSTEM/Administrators/セットアップ実行者のみアクセス可能に制限。
- **ログイン**: `POST /api/auth/login` → JWT（12時間）。連続失敗5回で60秒ロックアウト。
- **APIキー**: 管理者が発行（`kjk_` プレフィックス）。サーバーには SHA-256 ハッシュのみ保存。権限失効は即時削除。

### アクセス制御（RBAC）
| ロール | 権限 |
|---|---|
| viewer | メトリクス・ジョブ状態・ログ・ファイルの**読み取りのみ** |
| operator | ジョブ実行/停止、ターミナル利用、ファイル読み書き（ワークスペース内） |
| admin | 上記 + 設定変更・APIキー管理・監査ログ閲覧・トンネル制御 |

### リモートからの管理者権限実行を認めない設計
- kenji はリモートからのコマンドを**サービスアカウント権限でのみ**実行する（昇格・`runas`・UAC昇格APIは提供しない）。
- サービスアカウントは `kenji service install --account` で指定可能。**標準ユーザーアカウントを推奨**（LocalSystem 指定時は明示警告）。
- 全ての操作は監査ログ（誰が・いつ・何を）に記録され、adminが参照可能。
- ファイル操作はワークスペースルート配下に**サンドボックス化**（パストラバーサル拒否・正規化検証）。

### ネットワーク
- デフォルト `127.0.0.1` のみバインド。外部アクセスは Cloudflare Tunnel（TLS）経由のみ。
- 追加保護として Cloudflare Access（Zero Trustポリシー）の併用を推奨（ドキュメントで案内）。

---

## 4. データレイアウト

```
C:\ProgramData\kenji\
├── config.json      # ポート・ワークスペース・JWTシークレット・トンネル設定
├── auth.json        # 管理者(ハッシュ)・APIキー(ハッシュ) ※厳格ACL
├── jobs.json        # ジョブ定義（停止状態でも永続）
├── workspace\       # ファイルAPI・ジョブcwd のサンドボックスルート
├── logs\<jobid>.log # ジョブログ（5MBローテーション、.1を1世代保持）
├── audit\audit.jsonl # 監査ログ（10MBローテーション）
└── updates\          # アップデートステージング（kenji.exe.new・finalize.log）
```

## 5. 技術選定

| 領域 | 選択 | 理由 |
|---|---|---|
| バックエンド | Rust + axum + tokio | 高信頼・低フットプリント・単一バイナリ配布。非同期プロセス管理とWSに最適 |
| PTY | portable-pty（wezterm製） | ConPTYを成熟した実装で利用。クロスプラットフォーム |
| プロセス停止 | `taskkill /T /F`（子孫含む強制停止） | 追加FFI不要。Job Objectによる資源制限はPhase 2 |
| メトリクス | sysinfo + nvml-wrapper | 純Rust。GPU は NVIDIA がない環境では自動的に無効化 |
| Web UI | React + TypeScript + Tailwind（shadcn/uiスタイル）+ xterm.js + recharts | 要件通り。ビルド成果物をバイナリに埋め込み |
| サービス | windows-service クレート | SCM統合の定番 |
| トンネル | cloudflared（公式） | 要件指定 |

## 6. ロードマップ

- **MVP（本次実装）**: 認証/RBAC/監査、ジョブ実行・管理・再実行・ログ(WSライブテール)・cronスケジュール・再起動ポリシー、PowerShellターミナル、ファイルAPI、メトリクス(CPU/GPU/メモリ/ディスク)、Web UI、Windows Service、cloudflared管理（Quick Tunnel）、MCPサーバー、自動アップデート（GitHub Releases・SHA256検証）、セットアップCLI
- **Phase 2**: Job Objectによるメモリ/CPUレート制限、フォルダzipダウンロード、名前付きトンネル（独自ドメイン）、複数パブリックエンドポイント公開、APIクライアントCLI、バックアップ/リストア
- **Phase 3**: マルチノード（中央管理サーバー＋エージェント）、コンテナ統合UI、リソース割当クォータ、Cloudflare Access連携

## 7. MVPのトレードオフ（意図的なもの）

- ジョブ状態は JSON ファイル + インメモリ（SQLite はPhase 2で検討）— 定義永続はこれで十分
- cron は5フィールド標準形式、実行中の多重起動はスキップ（catch-upなし）
- リソース「制限」はプロセス停止まで（Job Object制限はPhase 2）
- トンネルは Quick Tunnel（アカウント不要・即時）。名前付きトンネルはドキュメントで案内

## 8. 自動アップデート

GitHub Releases を配布元とするセルフアップデート。リリースには
`kenji-windows-amd64.exe` と `.sha256` チェックサムアセットが必須で、
**チェックサムが一致しないリリースは一切適用しない**（.github/workflows/release.yml が両アセットを生成）。

フロー: `GET releases/latest` → semver比較 → ダウンロード（128MB上限）→ SHA256検証 →
実行中バイナリを `*.old` にリネーム（Windowsでは実行中exeのリネームは可能）し新バイナリを設置 →
graceful shutdown → デタッチした `kenji update-finalize` ヘルパーが旧プロセス停止を確認してから、
サービスモードならSCM経由で、フォアグラウンドなら同一引数で新バイナリを再起動。

- 設計判断: 更新確認はデフォルトON（24時間毎）、**適用は手動（admin API/UI）がデフォルト**。
  `auto_install` をONにすると検出→検証→適用→再起動まで完全自動。
- install API は admin 権限 + 監査ログ必須。ダウンロードは `updates\kenji.exe.new` にステージし、
  失敗時は稼働中インスタンスに影響なし。
- `KENJI_UPDATE_API` 環境変数でGitHub API互換のエンドポイントを差し替え可能（テスト用）。
