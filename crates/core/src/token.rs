use serde::{Deserialize, Serialize};

use crate::identity::TokenId;

/// `symbol` is for display only and must never enter pricing or routing logic.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenMeta {
    pub id: TokenId,
    pub decimals: Option<u8>,
    pub symbol: Option<String>,
}
