use agent2api_wasm_sdk::{
    http::{self, Response},
    json, Error, Result, Value,
};

pub const BASE: &str = "https://ai.soho.komect.com";
pub const USER: &str = "/ai/llm/agent/user/getUserInfo";
pub const BALANCE: &str = "/ai/llm/agent/user/getTokenBalance";
pub const REFRESH: &str = "/ai/llm/agent/user/agentRefreshToken";
pub const MODELS: &str = "/ai/llm/gw/v1/models/getAvailableModels";
pub const CHAT: &str = "/ai/llm/gw/v1/chat/completions";
pub const CAMPAIGNS: &str = "/ai/llm/order-center/public/client/v1/marketing-campaigns";

pub fn code(body: &Value) -> Option<i64> {
    body.get("code")
        .or_else(|| body.pointer("/error/code"))
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

pub fn auth(status: u16, code: Option<i64>) -> bool {
    status == 401 || matches!(code, Some(3004 | 1017 | 1018))
}

pub fn error(status: u16, body: &Value) -> Error {
    let code = code(body);
    let (status, message) = match code {
        Some(1012 | 1013) => (400, "移动灵犀要求图形验证码，请在官网完成登录后导入 Token"),
        Some(1014 | 1015) => (400, "短信验证码错误或已过期，请检查后重试"),
        Some(1016) => (429, "短信发送过于频繁，请稍后重试"),
        _ if auth(status, code) => (401, "移动灵犀登录已失效，请重新登录或更新凭证"),
        _ if matches!(status, 402 | 429) => (status, "移动灵犀额度不足或请求频繁"),
        _ => (
            if status < 400 { 502 } else { status },
            "移动灵犀上游请求失败",
        ),
    };
    let mut error = Error::http(status, message);
    error.upstream_code = code;
    error
}

pub fn open(path: &str, token: Option<&str>, body: &Value, chat: bool) -> Result<Response> {
    let mut headers = json!({"x-client-id":"lingxiclaw", "content-type":"application/json"});
    if let Some(token) = token {
        headers["authorization"] = json!(format!("Bearer {token}"));
    }
    let url = format!("{BASE}{path}");
    if chat {
        headers["accept"] = json!("text/event-stream, application/json");
        http::open("POST", &url, headers, Some(body))
    } else {
        http::open_timeout("POST", &url, headers, Some(body), 25_000)
    }
}

pub fn post(path: &str, token: Option<&str>, payload: &Value) -> Result<Value> {
    post_inner(path, token, payload, false)
}

pub fn send_sms(payload: &Value) -> Result<()> {
    post_inner("/ai/llm/agent/user/agentSendSmsCode", None, payload, true).map(|_| ())
}

fn post_inner(path: &str, token: Option<&str>, payload: &Value, ack: bool) -> Result<Value> {
    let response = open(path, token, payload, false)?;
    let status = response.status;
    if !(200..300).contains(&status) {
        return Err(error(status, &Value::Null));
    }
    let body: Value = serde_json::from_slice(&response.bytes(2 * 1024 * 1024)?)
        .map_err(|_| Error::http(502, "移动灵犀上游响应格式无效"))?;
    if !matches!(code(&body), Some(0 | 200)) {
        return Err(error(status, &body));
    }
    if ack {
        return Ok(Value::Null);
    }
    body.get("data")
        .filter(|v| !v.is_null())
        .cloned()
        .ok_or_else(|| Error::http(502, "移动灵犀上游响应缺少数据"))
}
