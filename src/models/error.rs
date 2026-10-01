use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("モデルが設定されていません（ベースURLとモデル名を入力してください）")]
    NotConfigured,
    #[error("HTTPクライアント (curl) を起動できませんでした: {0}")]
    Transport(#[from] std::io::Error),
    #[error("モデルに接続できませんでした: {0}")]
    Connection(String),
    #[error("モデルがエラーを返しました (HTTP {status}): {message}")]
    Http { status: u16, message: String },
    #[error("モデルの応答を解釈できませんでした: {0}")]
    InvalidOutput(String),
    #[error("{0}")]
    Unsupported(String),
}
