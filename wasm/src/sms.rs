use crate::{account, upstream};
use agent2api_wasm_sdk::{
    self as sdk, json,
    login::sms::{self, Config, CountryCode, Request},
    state::{self, Scope, Snapshot},
    Error, Result, Value,
};

const KEY: &str = "lingxi-sms";
const TTL: i64 = 300_000;
const RESEND: i64 = 60_000;

pub fn method() -> Value {
    sms::method(
        1,
        "手机号短信登录",
        Config {
            phone_label: "手机号".into(),
            phone_placeholder: "11 位中国大陆手机号".into(),
            country_codes: vec![CountryCode {
                value: "86".into(),
                label: "+86".into(),
            }],
            phone_min_length: 11,
            phone_max_length: 11,
            code_min_length: 6,
            code_max_length: 6,
            resend_after_seconds: 60,
            expires_after_seconds: 300,
        },
    )
}

fn parse(params: &Value) -> Result<(Request, String)> {
    if params["flow"].as_str().is_none_or(str::is_empty) {
        return Err(Error::http(400, "短信登录流程已失效，请重新发起"));
    }
    let request: Request = serde_json::from_value(params["payload"].clone())
        .map_err(|_| Error::http(400, "短信登录参数格式无效"))?;
    if request.method_index != 1 || request.country_code != "86" {
        return Err(Error::http(400, "仅支持中国大陆手机号"));
    }
    let phone = account::phone(&request.phone)?;
    Ok((request, phone))
}

fn load() -> Result<Snapshot> {
    let snapshot = state::get(Scope::Login, KEY, 1)?;
    if !snapshot.compatible {
        return Err(Error::http(409, "短信登录状态已变化，请重新发起"));
    }
    Ok(snapshot)
}

fn save(previous: &Snapshot, value: Value) -> Result<()> {
    if !state::compare_exchange(Scope::Login, KEY, 1, previous.revision, value, TTL)? {
        return Err(Error::http(409, "短信登录状态已变化，请重新发起"));
    }
    Ok(())
}

pub fn send(params: &Value) -> Result<Value> {
    let (_, phone) = parse(params)?;
    let current = load()?;
    let now = sdk::now();
    if current.value["retryAt"].as_i64().unwrap_or(0) > now {
        return Err(Error::http(429, "请等待 60 秒后重新发送验证码"));
    }
    if matches!(current.value["phase"].as_str(), Some("verifying" | "done")) {
        return Err(Error::http(409, "本次短信登录正在验证或已经完成"));
    }
    let ticket = sdk::host("random.bytes", json!({"length":18}))?;
    let value = json!({"phase":"sending","phone":phone,"ticket":ticket,
        "expiresAt":now.saturating_add(TTL),"retryAt":now.saturating_add(RESEND),"attempts":0});
    save(&current, value.clone())?;
    let reserved = load()?;
    if reserved.value != value {
        return Err(Error::http(409, "短信登录状态已变化"));
    }
    upstream::send_sms(&json!({"phone":phone}))?;
    let mut sent = value;
    sent["phase"] = json!("sent");
    save(&reserved, sent)?;
    Ok(sms::sent(ticket, 60, 300))
}

pub fn verify(params: &Value) -> Result<Value> {
    let (request, phone) = parse(params)?;
    if request.code.len() != 6 || !request.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::http(400, "请填写 6 位短信验证码"));
    }
    let current = load()?;
    if current.value["phase"] != "sent"
        || current.value["phone"] != phone
        || current.value["ticket"] != request.ticket
        || current.value["expiresAt"].as_i64().unwrap_or(0) <= sdk::now()
    {
        return Err(Error::http(400, "验证码未发送或已过期，请重新获取"));
    }
    let attempts = current.value["attempts"].as_u64().unwrap_or(0);
    if attempts >= 5 {
        return Err(Error::http(429, "验证码尝试过多，请重新获取"));
    }
    let mut value = current.value.clone();
    value["attempts"] = json!(attempts + 1);
    value["phase"] = json!("verifying");
    save(&current, value.clone())?;
    let reserved = load()?;
    if reserved.value != value {
        return Err(Error::http(409, "短信登录状态已变化"));
    }
    let login = upstream::post(
        "/ai/llm/agent/user/agentSmsLogin",
        None,
        &json!({"phone":phone,"sms_code":request.code,"platform":"web","agent_code":"lingxiclaw","device_uid":"default"}),
    );
    match login {
        Ok(data) => {
            let normalized =
                account::normalized(crate::validate(&data, Some(&account::hash(&phone)))?, None)?;
            save(&reserved, json!({"phase":"done"}))?;
            Ok(normalized)
        }
        Err(error) => {
            value["phase"] = json!("sent");
            save(&reserved, value)?;
            Err(error)
        }
    }
}
