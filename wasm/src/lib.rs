mod account;
mod common;
mod control;
mod openai;
mod sms;
mod upstream;

use agent2api_wasm_sdk::{
    self as sdk, catalog, json, refresh_guard::Reservation, Error, Result, Value,
};
use base64::{engine::general_purpose::STANDARD, Engine};

const ID: &str = "lingxi";
sdk::export_provider!(handle);

fn metadata() -> Value {
    json!({"id":ID,"label":"移动灵犀","order":20,
        "capabilities":{"chat":true,"refresh":true,"usage":true,"checkin":true,"web_login":true,
            "model_refresh":true,"sse_model_rewrite":true,"refresh_uses_account":true,"supports_default_model":false},
        "account_features":{"usage":true,"identifier":["phone"],"expiry":["expiresAt"]},
        "checkin":{"daily":true,"modes":["checkin"],"description":"领取 AI 每日畅想计划奖励"},
        "default_models":[]})
}

fn auth_methods() -> Value {
    let mut methods = account::methods();
    if let Some(methods) = methods["methods"].as_array_mut() {
        methods.push(sms::method());
    }
    methods
}

fn validate(payload: &Value, expected: Option<&str>) -> Result<Value> {
    let secret = account::secret(payload)?;
    let user = upstream::post(
        upstream::USER,
        Some(account::text(&secret, "access_token")),
        &json!({}),
    )?;
    account::bind(secret, &user, expected)
}

fn normalize(params: &Value) -> Result<Value> {
    let source = &params["source"];
    let bad = || Error::http(400, "请导入单个账号的有效凭证（最多 256 KiB）");
    let payload = match source["kind"].as_str() {
        Some("manual") => source["payload"].clone(),
        Some("file_text" | "file_base64") => {
            let raw = source["content"].as_str().ok_or_else(bad)?.trim();
            let decoded;
            let content = if source["kind"] == "file_base64" {
                if raw.len() > (256 * 1024usize).div_ceil(3) * 4 {
                    return Err(bad());
                }
                decoded = STANDARD.decode(raw).map_err(|_| bad())?;
                std::str::from_utf8(&decoded).map_err(|_| bad())?
            } else {
                raw
            };
            if content.len() > 256 * 1024 {
                return Err(bad());
            }
            account::file(content)?
        }
        _ => return Err(Error::unsupported()),
    };
    let payload = account::manual(payload)?;
    account::normalized(
        validate(&payload, None)?,
        params["name"].as_str().or_else(|| payload["name"].as_str()),
    )
}

fn identity(secret: &Value) -> Result<String> {
    let id = account::text(secret, "identity");
    if id.is_empty() {
        return Err(Error::http(401, "移动灵犀账户缺少已验证身份，请重新登录"));
    }
    Ok(format!("{ID}:{id}"))
}

fn expiring(secret: &Value, lead_ms: i64) -> bool {
    !account::text(secret, "refresh_token").is_empty()
        && account::expires(&secret["access_token_expires_at"])
            .is_some_and(|at| at.saturating_mul(1000) <= sdk::now().saturating_add(lead_ms))
}

/// Caller holds the account lock. Reads and updates use the exact account bound by the host.
fn ready(force: bool) -> Result<String> {
    let stored = common::account()?;
    let secret = &stored["secret"];
    let access = account::token(account::text(secret, "access_token"))?;
    if !force && !expiring(secret, 90_000) {
        return Ok(access);
    }
    let refresh = account::text(secret, "refresh_token");
    if refresh.is_empty() {
        return Err(Error::http(
            401,
            "此账号缺少 Refresh Token，请重新登录或导入凭证",
        ));
    }
    let identity = account::text(secret, "identity");
    if identity.is_empty() {
        return Err(Error::http(401, "此账号缺少已验证身份，请重新登录"));
    }
    let reservation = Reservation::acquire("refresh-attempt", &format!("{access}\n{refresh}"))?;
    let mut data = match upstream::post(
        upstream::REFRESH,
        Some(&access),
        &json!({"refresh_token":refresh}),
    ) {
        Ok(data) => data,
        Err(error) => {
            if error.status == Some(401) {
                reservation.complete()?;
            }
            return Err(error);
        }
    };
    if !data.is_object() {
        return Err(Error::http(502, "移动灵犀续期响应格式无效"));
    }
    if account::text(&data, "refresh_token").is_empty() {
        data["refresh_token"] = json!(refresh);
        if let Some(expiry) = secret.get("refresh_token_expires_at") {
            data["refresh_token_expires_at"] = expiry.clone();
        }
    }
    let mut next = validate(&data, Some(identity))?;
    next["name"] = secret["name"].clone();
    let update = sdk::host(
        "account.update",
        json!({"expect":secret,"secret":next,"public":account::public(&next)}),
    )?;
    if update["updated"] != true {
        return Err(Error::http(409, "续期期间账号已发生变化，请重试"));
    }
    reservation.complete()?;
    account::token(account::text(&next, "access_token"))
}

fn token(force: bool) -> Result<String> {
    let _guard = sdk::AccountLock::acquire()?;
    ready(force)
}

fn business(path: &str) -> Result<Value> {
    let used = token(false)?;
    match upstream::post(path, Some(&used), &json!({})) {
        Err(error) if error.status == Some(401) => {
            let _guard = sdk::AccountLock::acquire()?;
            let current = common::account()?;
            let current = account::text(&current["secret"], "access_token");
            let next = if current == used {
                ready(true)?
            } else {
                account::token(current)?
            };
            upstream::post(path, Some(&next), &json!({}))
        }
        reply => reply,
    }
}

fn usage() -> Result<Value> {
    let data = business(upstream::BALANCE)?;
    let value = &data["compute_credit"];
    let n = value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
        .ok_or_else(|| Error::http(502, "移动灵犀余额缺少有效 compute_credit"))?;
    Ok(
        json!({"provider":ID,"plan":"移动灵犀算力豆","available":n,"balance":n,"unit":"算力豆",
        "windows":[{"name":"算力豆","label":"算力豆","remaining":n,"balance":n,"unit":"算力豆","display":n.to_string()}]}),
    )
}

fn conversation(mut params: Value) -> Result<()> {
    let (body, _) = openai::body(&mut params)?;
    let token = token(false)?;
    let response = upstream::open(upstream::CHAT, Some(&token), &body, true)?;
    openai::relay(response, upstream::error)
}

fn handle(method: &str, params: Value) -> Result<Value> {
    match method {
        "init" => Ok(json!([metadata()])),
        "provider.info" => Ok(metadata()),
        "ping" => Ok(json!(true)),
        "admin.action" => match params["action"].as_str() {
            Some("auth.methods") => Ok(auth_methods()),
            Some("login.sms.send") => sms::send(&params),
            Some("login.sms.verify") => sms::verify(&params),
            _ => Err(Error::unsupported()),
        },
        "account.normalize" => normalize(&params),
        "account.view" => Ok(account::public(&params["secret"])),
        "account.identity" => Ok(json!(identity(&params["secret"])?)),
        "token.ensure" | "token.refresh" => Ok(json!(token(method == "token.refresh")?)),
        "token.expiring" => Ok(json!(expiring(
            &common::account()?["secret"],
            sdk::expiring_lead(&params, 90_000)
        ))),
        "models.list" => catalog::cached(),
        "models.refresh" => control::models(),
        "usage.query" => usage(),
        "checkin.claim" => control::daily(),
        "conversation" => {
            conversation(params)?;
            Ok(Value::Null)
        }
        "classify" => {
            let status = params["status"].as_u64().unwrap_or(502) as u16;
            let body = &params["body"];
            let error = upstream::error(status, body);
            Ok(common::classify(
                error.status.unwrap_or(502),
                error.upstream_code,
                upstream::auth(status, error.upstream_code),
                &error.message,
            ))
        }
        "is_error_response" => Ok(json!(params["status"].as_u64().unwrap_or(502) >= 400)),
        _ => Err(Error::unsupported()),
    }
}
