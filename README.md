# LINE Bot sample (Rust + Axum + Cloud Run)

このプロジェクトは、LINE Messaging API の Webhook を受け取り、
受信したテキストメッセージをオウム返しする、最小構成の Rust サーバです。

- Web フレームワーク: [axum](https://github.com/tokio-rs/axum)
- HTTP クライアント: reqwest
- デプロイ先想定: Google Cloud Run

## 環境変数

実行時に以下の環境変数が必要です。

- `LINE_CHANNEL_SECRET` : LINE Developers の Messaging API チャネル設定画面に表示される Channel secret
- `LINE_CHANNEL_ACCESS_TOKEN` : Messaging API チャネルの「チャネルアクセストークン（ロングターム）」
- `GCS_BUCKET` : 画像を置く GCS バケット名
- `ADMIN_USER_IDS` : 画像アップロードを許可する LINE ユーザー ID（カンマ区切り）
- `SERVICE_ACCOUNT_JSON` もしくは `GOOGLE_APPLICATION_CREDENTIALS_JSON` : GCS にアクセスするサービスアカウントの JSON（または `GOOGLE_APPLICATION_CREDENTIALS` で JSON ファイルへのパス）
- `PORT` (任意) : サーバが listen するポート番号。Cloud Run では自動で `PORT` が渡されるので、通常は設定不要です。ローカル実行時などは未設定なら `8080` が使われます。

## ローカル実行

```bash
export LINE_CHANNEL_SECRET="xxxxxxxxxxxxxxxx"
export LINE_CHANNEL_ACCESS_TOKEN="xxxxxxxxxxxxxxxx"
export PORT=8080

cargo run
```

ngrok 等でローカルサーバを公開し、その URL + `/webhook` を
LINE Developers コンソールの Webhook URL に設定すると動作確認できます。

例: `https://<YOUR_NGROK_ID>.ngrok.io/webhook`

## Cloud Run 用 Docker イメージのビルド

```bash
# プロジェクトIDは適宜置き換えてください
export PROJECT_ID=your-gcp-project-id
export REGION=asia-northeast1
export SERVICE_NAME=line-bot-rust

gcloud builds submit --tag gcr.io/$PROJECT_ID/$SERVICE_NAME
```

## Cloud Run へのデプロイ

```bash
gcloud run deploy $SERVICE_NAME       --image gcr.io/$PROJECT_ID/$SERVICE_NAME       --platform managed       --region $REGION       --allow-unauthenticated       --set-env-vars LINE_CHANNEL_SECRET=xxxxxxxxxxxxxxxx,LINE_CHANNEL_ACCESS_TOKEN=xxxxxxxxxxxxxxxx
```

デプロイ後に表示される URL を、LINE Developers コンソールの
「Messaging API」設定画面にある Webhook URL に設定し、有効化してください。

例: `https://line-bot-rust-xxxxxxxxx-uc.a.run.app/webhook`

### Cloud Run でのサービスアカウント鍵の渡し方

`cloud-storage` クレートはサービスアカウントの JSON を環境変数経由で読むため、Cloud Run でも `SERVICE_ACCOUNT_JSON`（または `GOOGLE_APPLICATION_CREDENTIALS_JSON` / `GOOGLE_APPLICATION_CREDENTIALS`）を設定する必要があります。鍵ファイルは Secret Manager に格納し、環境変数として注入するのがおすすめです。

例（Secret Manager の `gcs-sa-key` に鍵を保存した場合）:

```bash
gcloud run deploy $SERVICE_NAME \
  --image gcr.io/$PROJECT_ID/$SERVICE_NAME \
  --platform managed \
  --region $REGION \
  --allow-unauthenticated \
  --set-env-vars LINE_CHANNEL_SECRET=xxxxxxxxxxxxxxxx,LINE_CHANNEL_ACCESS_TOKEN=xxxxxxxxxxxxxxxx,GCS_BUCKET=your-bucket-name \
  --set-secrets SERVICE_ACCOUNT_JSON=gcs-sa-key:latest
```

## 動作概要

1. LINE から Webhook イベントを受信
2. `x-line-signature` ヘッダとリクエストボディから署名検証
3. メッセージイベント & テキストメッセージであれば
   `replyToken` に対してオウム返しメッセージを送信

あとはこの骨組みをベースに、店舗ごとのメニュー表示ロジックなどを
追加していく想定です。
