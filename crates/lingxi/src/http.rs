use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::{control_error, PluginCtx, StoreAccount, StoreProxy};
use reqwest::{Client, Url};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub const BASE: &str = "https://ai.soho.komect.com";
pub const USER: &str = "/ai/llm/agent/user/getUserInfo";
pub const BALANCE: &str = "/ai/llm/agent/user/getTokenBalance";
pub const REFRESH: &str = "/ai/llm/agent/user/agentRefreshToken";
pub const MODELS: &str = "/ai/llm/gw/v1/models/getAvailableModels";
pub const CHAT: &str = "/ai/llm/gw/v1/chat/completions";
pub const CAMPAIGNS: &str = "/ai/llm/order-center/public/client/v1/marketing-campaigns";
pub fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64 }
pub fn resolve_proxy(ctx: &PluginCtx, stored: &StoreAccount, explicit: Option<&proto::WireProxy>) -> Result<String, String> {
    if agent2api_provider_sdk::extensions::direct_proxy_override() { return Ok("direct".into()); }
    if let Some(proxy) = explicit { return Ok(proxy.to_url()); }
    match stored.proxy.as_ref() {
        Some(StoreProxy::Resolved { proxy }) => Ok(proxy.to_url()),
        Some(StoreProxy::Failed { .. }) => Err("LOCAL_PRECONDITION:移动灵犀 账号代理不可用，请检查账号代理设置".into()),
        Some(StoreProxy::Static { config }) => static_proxy(config),
        _ => Ok(ctx.env.proxy.clone()),
    }
}

// The gateway deliberately sends explicit direct and legacy URL settings as
// Static. Only pool/rotation/Clash settings need a gateway-resolved endpoint.
fn static_proxy(config: &Value) -> Result<String, String> {
    let invalid = || "LOCAL_PRECONDITION:移动灵犀 账号静态代理配置无效，请检查代理地址、协议和端口".to_string();
    let config = config.get("config").filter(|v| v.is_object()).unwrap_or(config);
    let source = config.get("source").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    if source == "direct" { return Ok("direct".into()); }
    if matches!(source.as_str(), "system" | "pool" | "rotate" | "clash" | "subscription") {
        return Err("LOCAL_PRECONDITION:移动灵犀 账号代理缺少网关解析的出口，请检查代理来源或更新网关".into());
    }
    if !config.is_object() || config.get("error").is_some_and(|v| !v.is_null()) { return Err(invalid()); }
    let candidate = if let Some(host) = config.get("host").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        let protocol = config.get("protocol").and_then(Value::as_str).unwrap_or("http").trim().to_lowercase();
        let port = match config.get("port") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()).and_then(|v| u16::try_from(v).ok()).filter(|v| *v > 0).ok_or_else(invalid)?),
        };
        let host = host.trim_matches(|ch| ch == '[' || ch == ']');
        let host = if host.contains(':') { format!("[{host}]") } else { host.into() };
        proto::WireProxy { protocol, host, port, username: config.get("username").and_then(Value::as_str).unwrap_or("").into(), password: config.get("password").and_then(Value::as_str).unwrap_or("").into() }.to_url()
    } else {
        ["url", "proxy", "address", "server"].iter().find_map(|key| config.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())).ok_or_else(invalid)?.into()
    };
    let url = Url::parse(&candidate).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https" | "socks4" | "socks5" | "socks5h") || url.host_str().is_none() || url.port() == Some(0) || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() { return Err(invalid()); }
    reqwest::Proxy::all(url.as_str()).map_err(|_| invalid())?;
    Ok(url.into())
}

pub(crate) fn endpoint(variable: &str, fallback: &str) -> Result<String, String> {
    match std::env::var(variable) {
        Ok(value) => {
            let parsed = Url::parse(&value).map_err(|_| "移动灵犀 测试端点地址无效")?;
            if parsed.scheme() != "http" || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) || parsed.port().is_none() || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
                return Err("移动灵犀 测试端点只允许 http://127.0.0.1:端口".into());
            }
            Ok(value.trim_end_matches('/').into())
        }
        Err(_) => Ok(fallback.into()),
    }
}

pub fn base() -> Result<String, String> { endpoint("LINGXI_TEST_BASE", BASE) }
pub fn login_proxy(ctx: &PluginCtx, value: Option<&Value>) -> Result<String, String> {
    if agent2api_provider_sdk::extensions::direct_proxy_override() { return Ok("direct".into()); }
    match value.filter(|v| !v.is_null()) {
        Some(v) => Ok(serde_json::from_value::<proto::WireProxy>(v.clone()).map_err(|_| "移动灵犀代理参数无效")?.to_url()),
        None => Ok(ctx.env.proxy.clone()),
    }
}
pub struct Failure { pub status: u16, pub code: i64 }
impl Failure {
    pub fn auth(&self) -> bool { self.status == 401 || [3004, 1017, 1018].contains(&self.code) }
    pub fn message(&self) -> String {
        let (status, text) = match self.code {
            1012 | 1013 => (400, "请填写或刷新图形验证码"),
            1014 | 1015 => (400, "短信验证码错误或已过期，请检查后重试"),
            1016 => (429, "短信发送过于频繁，请稍后重试"),
            _ if self.auth() => (401, "移动灵犀登录已失效，请重新登录或更新凭证"),
            _ if self.status == 429 || self.status == 402 => (429, "移动灵犀额度不足或请求频繁"),
            _ => (502, "移动灵犀上游请求失败，请检查网络、代理或稍后重试"),
        };
        control_error(status, text, None, Some(self.code))
    }
}
pub async fn post(proxy: &str, path: &str, token: Option<&str>, payload: &Value) -> Result<Value, Failure> {
    post_inner(proxy, path, token, payload, false).await
}

/// SMS delivery is an acknowledgement: code 0/200 is sufficient even when data
/// is missing or null. Other endpoints must still return their documented data.
pub async fn send_sms(proxy: &str, payload: &Value) -> Result<(), Failure> {
    post_inner(proxy, "/ai/llm/agent/user/agentSendSmsCode", None, payload, true).await.map(|_| ())
}

async fn post_inner(proxy: &str, path: &str, token: Option<&str>, payload: &Value, acknowledgement: bool) -> Result<Value, Failure> {
    let mut builder = Client::builder().no_proxy().timeout(Duration::from_secs(25)).connect_timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none());
    if !proxy.is_empty() && proxy != "direct" { builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| Failure { status: 502, code: -1 })?); }
    let client = builder.build().map_err(|_| Failure { status: 502, code: -1 })?;
    let url = format!("{}{}", base().map_err(|_| Failure { status: 502, code: -1 })?, path);
    let mut request = client.post(url).header("X-Client-Id", "lingxiclaw").json(payload);
    if let Some(token) = token { request = request.bearer_auth(token); }
    let mut response = request.send().await.map_err(|_| Failure { status: 502, code: -1 })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) { return Err(Failure { status, code: 0 }); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure { status: 502, code: -1 })? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 { return Err(Failure { status: 502, code: -2 }); }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| Failure { status: 502, code: -2 })?;
    let code = body.get("code").and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok())).ok_or(Failure { status: 502, code: -2 })?;
    if code != 0 && code != 200 { return Err(Failure { status, code }); }
    if acknowledgement { return Ok(Value::Null); }
    body.get("data").filter(|v| !v.is_null()).cloned().ok_or(Failure { status: 502, code: -2 })
}
