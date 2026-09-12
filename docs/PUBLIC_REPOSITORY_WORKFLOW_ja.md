# 公開リポジトリ運用方針

この文書は、この Win-CodexBar fork における Git 運用と公開境界を定義します。

## ブランチ構成

- `main`: 公開可能な安定版。
- `develop`: 開発中だが公開されても問題ないコードを統合するブランチ。
- `feature/*`: `develop` から作成する機能単位の作業ブランチ。
- 昇格順序は `feature/* -> develop -> main` とします。

ブランチは開発状態と機能分離のために使用し、秘密情報の分離には使用しません。

## Repository / remote の役割

- `origin`: このプロジェクトが管理する GitHub Public Repository。
- `windows-upstream`: Windows / Rust / Tauri 版のコード系統で基準とする upstream。
- `upstream`: 元の CodexBar repository。参照・比較用。
- `private`: 内部専用ソースコードを別の Private Repository に置く必要がある場合のみ追加します。通常のローカル設定や秘密情報の保存には使用しません。

## 公開・非公開の境界

Public Repository には、Git 履歴を含めて秘密情報やPC固有の非公開情報を入れません。

Git 管理外とする代表例:

- `.env` / `.env.*`
- `config.local.json` / `local-config.json`
- `local/`
- `secrets/`
- `*.key` / `*.pem`
- `.cargo-home/` / `.orchestrator/` / `.pnpm-store/` などのローカルツール・キャッシュ

共有可能な設定例が必要な場合は、`config.example.json` のようなダミー・サニタイズ済みのファイルだけを Git 管理します。実際の認証情報やPC固有値は Git の外に置きます。

API Key / Token はブランチやPrivate Repositoryではなく、SOPS等の外部Secret Storeで管理します。

## プロジェクト構成

Win-CodexBar は既存 upstream との互換性を保つため、一般的なサンプルに合わせる目的だけで `src/` 等へ物理移動はしません。ただし公開境界は同じです。

- アプリケーションソースとテストは公開対象
- 製品ドキュメントは公開対象
- ローカル設定と秘密情報はGit管理外

既存の root `README.md` と `README.ja-JP.md` は upstream の慣例に合わせて維持します。

## Public push 前チェック

1. tracked / staged file が意図したものだけか確認する。
2. push対象commit範囲とGit履歴を、秘密値・Token・Private Key・Cookie・個人メール・PC固有パス/hostについて検査する。
3. 公開設定サンプルがダミー値・サニタイズ済みであることを確認する。
4. `git diff --check` を実行する。
5. 必要なテストとpr`.orchestrator/` / `.pnpm-store/` などのローカルツール・キャッシュ

共有可能な設定例が必要な場合は、`config.example.json` のようなダミー・サニタイズ済みのファイルだけを Git 管理します。実際の認証情報やPC固有値は Git の外に置きます。

API Key / Token はブランチやPrivate Repositoryではなく、SOPS等の外部Secret Storeで管理します。

## プロジェクト構成

Win-CodexBar は既存 upstream との互換性を保つため、一般的なサンプルに合わせる目的だけで `src/` 等へ物理移動はしません。ただし公開境界は同じです。

- アプリケーションソースとテストは公開対象
- 製品ドキュメントは公開対象
- ローカル設定と秘密情報はGit管理外

既存の root `README.md` と `README.ja-JP.md` は upstream の慣例に合わせて維持します。

## Public push 前チェック

1. tracked / staged file が意図したものだけか確認する。
2. push対象commit範囲とGit履歴を、秘密値・Token・Private Key・Cookie・個人メール・PC固有パス/hostについて検査する。
3. 公開設定サンプルがダミー値・サニタイズ済みであることを確認する。
4. `git diff --check` を実行する。
5. 必要なテストとproduction buildを実行する。
6. 利用可能な場合、GitHub Secret Scanning / Push Protection が有効であることを確認する。
7. 開発中の変更は `feature/*` または `develop` へpushし、レビュー後にのみ `main` へ昇格する。

Private Repository が必要なのは内部専用ソースコードを分離する場合です。Secret Storeの代替として使用しません。
