use agent2api_provider_protocol as proto;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn hash(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str { v.get(key).and_then(Value::as_str).unwrap_or("").trim() }
pub fn phone(value: &str) -> Result<String, String> {
    let value = value.trim().strip_prefix("+86").unwrap_or(value.trim());
    if value.len() == 11 && value.starts_with('1') && value.bytes().all(|b| b.is_ascii_digit()) { Ok(value.into()) }
    else { Err("请填写 11 位手机号".into()) }
}
pub fn masked(value: &str) -> String {
    if phone(value).is_ok() { format!("{}****{}", &value[..3], &value[7..]) } else { String::new() }
}
pub fn token(value: &str) -> Result<String, String> {
    let value = value.trim();
    let value = if value.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("bearer ")) { value[7..].trim() } else { value };
    if value.is_empty() || value.len() > 32768 || value.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) { Err("请填写有效的 Access Token（不是工作台 API Key）".into()) } else if value.starts_with("lx_") { Err("请填写移动灵犀云端 Access Token，不能使用原工作台的 lx_ API Key".into()) } else { Ok(value.into()) }
}
pub fn expires(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok()).filter(|v| *v > 0).map(|v| if v > 10_000_000_000 { v / 1000 } else { v })
}
pub fn secret(input: &Value) -> Result<Value, String> {
    let mut source = input.clone();
    if source.get("access_token").is_none() && source.get("token").is_some() { source["access_token"] = source["token"].clone(); }
    let access = token(text(&source, "access_token"))?;
    let refresh = text(&source, "refresh_token");
    if !refresh.is_empty() { token(refresh)?; }
    let mut out = json!({"access_token":access, "refresh_token":refresh});
    for field in ["access_token_expires_at", "refresh_token_expires_at"] {
        if let Some(expiry) = source.get(field).and_then(expires) { out[field] = json!(expiry); }
    }
    if let Some(v) = source.get("identity").and_then(Value::as_str).filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())) { out["identity"] = json!(v); }
    if let Ok(p) = phone(text(&source, "phone")) { out["phone"] = json!(p); }
    Ok(out)
}
pub fn bind(mut secret: Value, user: &Value, expected: Option<&str>) -> Result<Value, String> {
    let p = phone(text(user, "phone")).ok();
    let uid = user.get("id").filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()));
    let identity = p.as_ref().or(uid.as_ref()).filter(|s| !s.is_empty()).map(|s| hash(s)).ok_or("移动灵犀未返回可识别的账户身份")?;
    if expected.is_some_and(|e| e != identity) { return Err("移动灵犀账户身份不一致，请另建账户".into()); }
    secret["identity"] = json!(identity);
    if let Some(p) = p { secret["phone"] = json!(p); }
    Ok(secret)
}
pub fn public(secret: &Value) -> Value {
    let mut out = json!({"phone":masked(text(secret,"phone")),"name":text(secret,"name"),"hasRefreshToken":!text(secret,"refresh_token").is_empty()});
    if let Some(expiry) = secret.get("access_token_expires_at").and_then(expires) { out["expiresAt"] = json!(expiry * 1000); }
    out
}
pub fn normalized(mut secret: Value, name: Option<&str>) -> Result<proto::NormalizedAccount, String> {
    let id = text(&secret,"identity").to_string();
    if id.is_empty() { return Err("移动灵犀账户缺少已验证身份".into()); }
    secret["name"] = json!(name.unwrap_or("").trim().chars().take(100).collect::<String>());
    Ok(proto::NormalizedAccount { public:public(&secret),identity:format!("lingxi:{id}"),secret,desktop:false })
}
pub fn methods() -> Value {
    json!({"instructions":"短信登录请打开侧栏「移动灵犀登录」。也可粘贴移动灵犀官网的 Access Token；Refresh Token 可用于续期。", "methods":[
        {"methodIndex":0,"type":"api","label":"Access / Refresh Token","prompts":[
            {"key":"access_token","type":"text","label":"Access Token","secret":true,"optional":false,"placeholder":"移动灵犀官网登录后取得的 access_token"},
            {"key":"refresh_token","type":"text","label":"Refresh Token（可选）","secret":true,"optional":true}]},
        {"methodIndex":1,"type":"oauth","label":"短信登录（侧栏页面）","localOnly":true,"manualCallback":true,"instructions":"请使用侧栏「移动灵犀登录」完成发短信、图形验证和输入短信码。","prompts":[
            {"key":"phone","type":"text","label":"手机号","secret":false,"optional":false}]}
    ]})
}
pub fn manual(input: Value) -> Result<Value, String> {
    if let Some(index) = input.get("methodIndex") {
        if index.as_u64() != Some(0) { return Err("请在侧栏「移动灵犀登录」完成短信登录".into()); }
        let inputs = input.get("inputs").and_then(Value::as_object).ok_or("账号表单格式无效")?;
        if inputs.iter().any(|(k,v)| !["access_token","refresh_token"].contains(&k.as_str()) || !v.is_string()) { return Err("账号表单字段无效".into()); }
        Ok(Value::Object(inputs.clone()))
    } else if input.is_object() && input.get("inputs").is_none() { Ok(input) }
    else { Err("账号凭证必须为 JSON 对象".into()) }
}
pub fn file(content: &str) -> Result<Value, String> {
    let content = content.trim().trim_start_matches('\u{feff}');
    if content.starts_with('{') { serde_json::from_str(content).map_err(|_| "账号文件 JSON 格式无效".into()) } else { Ok(json!({"access_token":content})) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn tokens_and_public_are_separate() {
        let s = secret(&json!({"access_token":"Bearer fixture","refresh_token":"refresh","phone":"13800138000"})).unwrap();
        assert_eq!(s["access_token"], "fixture");
        let view = public(&s).to_string();
        for word in ["fixture", "13800138000", "\"refresh\""] { assert!(!view.contains(word)); }
        assert!(token("bad\r\nheader").is_err()); assert!(token("lx_workbench-key").is_err());
    }
    #[test] fn binding_rejects_another_user() {
        let a = bind(json!({}), &json!({"phone":"13800138000"}), None).unwrap();
        assert!(bind(json!({}), &json!({"phone":"13800138001"}), a["identity"].as_str()).is_err());
        assert_eq!(expires(&json!(1790000000000_i64)), Some(1790000000));
    }
}
