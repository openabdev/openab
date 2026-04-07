use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: u64,
    pub method: String,
    pub params: Option<Value>,
}

pub struct Session {
    pub cwd: String,
    pub messages: Vec<Value>,
}
