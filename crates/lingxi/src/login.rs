//! SMS steps travel through the existing login callback channel; credentials only return to the gateway.
use crate::{account, http};
use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::PluginCtx;
use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use tokio::sync::Mutex;
const TTL: Duration = Duration::from_secs(300);
#[derive(Default)]
pub struct Manager { flows: Mutex<HashMap<String, Arc<Flow>>>, limits: Mutex<HashMap<String, Instant>> }
struct Flow { created: Instant, cancelled: AtomicBool, state: Mutex<State> }
struct State { phone: String, proxy: String, hint: Value, captcha: String, sent: bool, attempts: u8 }
fn pending(hint: &Value) -> proto::LoginPoll { proto::LoginPoll::Pending { hint: Some(hint.to_string()) } }
fn failed(message: &str) -> proto::LoginPoll { proto::LoginPoll::Failed { message:message.into() } }
impl Manager {
    pub async fn start(&self, ctx: &PluginCtx, payload: &Value) -> Result<proto::LoginStart,String> {
        if payload.get("methodIndex").and_then(Value::as_u64) != Some(1) { return Err("请打开侧栏「移动灵犀登录」，填写手机号后开始".into()); }
        let phone = account::phone(account::text(&payload["inputs"], "phone"))?;
        let proxy = http::login_proxy(ctx,payload.get("proxy"))?;
        let mut flows = self.flows.lock().await;
        flows.retain(|_,f| { if f.created.elapsed() >= TTL { f.cancelled.store(true, Ordering::SeqCst); false } else { true } });
        if flows.len() >= 32 { return Err("等待中的登录过多，请先取消旧流程".into()); }
        let id: String = rand::thread_rng().sample_iter(&Alphanumeric).take(40).map(char::from).collect();
        let hint = json!({"status":"ready","message":"请发送短信验证码"});
        flows.insert(id.clone(), Arc::new(Flow { created:Instant::now(),cancelled:AtomicBool::new(false),state:Mutex::new(State { phone,proxy,hint:hint.clone(),captcha:String::new(),sent:false,attempts:0 }) }));
        Ok(proto::LoginStart { flow:id,url:String::new(),instructions:hint.to_string(),poll_interval_ms:1000,external_browser:false,allowed_hosts:vec![] })
    }
    async fn get(&self, id: &str) -> Option<Arc<Flow>> { self.flows.lock().await.get(id).cloned() }
    pub async fn cancel(&self,id:&str) { if let Some(f)=self.flows.lock().await.remove(id) { f.cancelled.store(true,Ordering::SeqCst); } }
    pub async fn poll(&self,id:&str) -> proto::LoginPoll {
        let Some(flow)=self.get(id).await else { return failed("登录已结束，请重新开始"); };
        if flow.cancelled.load(Ordering::SeqCst) || flow.created.elapsed() >= TTL { self.cancel(id).await; return failed("登录已取消或过期，请重新开始"); }
        let state=flow.state.lock().await;
        pending(&state.hint)
    }
    pub async fn callback(&self, callback:proto::LoginCallback) -> proto::LoginPoll {
        let Some(flow)=self.get(&callback.flow).await else { return failed("登录已结束，请重新开始"); };
        let mut state=flow.state.lock().await;
        if flow.cancelled.load(Ordering::SeqCst) || flow.created.elapsed() >= TTL { self.cancel(&callback.flow).await; return failed("登录已取消或过期，请重新开始"); }
        let command:Value=match serde_json::from_str(&callback.code) { Ok(v)=>v,Err(_)=>return pending(&json!({"status":"error","message":"请在侧栏「移动灵犀登录」继续操作"})) };
        let action=account::text(&command,"action");
        let result: Result<Value,http::Failure> = match action {
            "captcha" => http::post(&state.proxy,"/ai/llm/agent/user/agentSendCaptcha",None,&json!({})).await,
            "send" => {
                let code=account::text(&command,"captcha_code");
                if (!state.captcha.is_empty() && code.is_empty()) || code.len()>16 { state.hint=json!({"status":"captcha_required","message":"请填写图形验证码"}); return pending(&state.hint); }
                let key=account::hash(&state.phone);
                { let mut limits=self.limits.lock().await;
                  limits.retain(|_,last|last.elapsed()<Duration::from_secs(60));
                  if limits.contains_key(&key) || limits.len()>=256 { state.hint=json!({"status":"rate_limited","message":"请等待 60 秒后再发送"});return pending(&state.hint); }
                  limits.insert(key,Instant::now()); }
                let mut body=json!({"phone":state.phone});
                if !state.captcha.is_empty() { body["captcha_id"]=json!(state.captcha); body["captcha_code"]=json!(code); }
                http::send_sms(&state.proxy,&body).await.map(|()| Value::Null)
            },
            "verify" => {
                let code=account::text(&command,"sms_code");
                if !state.sent || code.len()!=6 || !code.bytes().all(|b|b.is_ascii_digit()) { state.hint=json!({"status":"error","message":"请先发送短信，并填写 6 位短信验证码"});return pending(&state.hint); }
                state.attempts+=1;
                if state.attempts>5 { self.cancel(&callback.flow).await;return failed("验证码尝试过多，请重新开始登录"); }
                let mut body=json!({"phone":state.phone,"sms_code":code,"platform":"web","agent_code":"lingxiclaw","device_uid":"default"});
                if !state.captcha.is_empty() {
                    let code=account::text(&command,"captcha_code");
                    if code.is_empty() || code.len()>16 { state.hint=json!({"status":"captcha_required","message":"请填写图形验证码"});return pending(&state.hint); }
                    body["captcha_id"]=json!(state.captcha);body["captcha_code"]=json!(code);
                }
                http::post(&state.proxy,"/ai/llm/agent/user/agentSmsLogin",None,&body).await
            },
            _ => { state.hint=json!({"status":"error","message":"登录操作无效，请使用侧栏「移动灵犀登录」"});return pending(&state.hint); }
        };
        if flow.cancelled.load(Ordering::SeqCst) || flow.created.elapsed()>=TTL { self.cancel(&callback.flow).await;return failed("登录已取消或过期，请重新开始"); }
        match result {
            Err(error)=> {
                let (status,message)=match error.code {
                    1012|1013=>("captcha_required","请获取并填写图形验证码后重新发送"),
                    1014|1015=>("invalid_code","短信验证码错误或已过期，请重新输入"),
                    1016=>("rate_limited","发送过于频繁，请稍后重试"),
                    _=>("error","登录请求失败，请检查网络或代理后重试"),
                };
                state.hint=json!({"status":status,"message":message});pending(&state.hint)
            },
            Ok(data)=> match action {
                "captcha"=> {
                    let image=account::text(&data,"captcha_image");
                    let id=account::text(&data,"captcha_id");
                    if id.is_empty() || id.len()>512 || image.len()>14000 || !["data:image/png;base64,","data:image/jpeg;base64,","data:image/gif;base64,","data:image/webp;base64,"].iter().any(|p|image.starts_with(p)) { state.hint=json!({"status":"error","message":"上游图形验证码格式不支持，请刷新或使用 Token 导入"}); }
                    else { state.captcha=id.into();state.hint=json!({"status":"captcha","captcha_image":image,"message":"请输入图片中的验证码"}); }
                    pending(&state.hint)
                },
                "send"=>{state.sent=true;state.attempts=0;state.hint=json!({"status":"sent","message":"短信已发送，请输入 6 位验证码"});pending(&state.hint)},
                "verify"=> {
                    let normalized=async {
                        let mut secret=account::secret(&data)?;
                        // Verify the actual cloud identity; never bind a token to a caller-supplied phone.
                        let user=http::post(&state.proxy,http::USER,Some(account::text(&secret,"access_token")),&json!({})).await.map_err(|e|e.message())?;
                        let expected=account::hash(&state.phone);
                        secret=account::bind(secret,&user,Some(&expected))?;
                        account::normalized(secret,Some(account::text(&command,"name")))
                    }.await;
                    if flow.cancelled.load(Ordering::SeqCst) || flow.created.elapsed()>=TTL { self.cancel(&callback.flow).await;return failed("登录已取消或过期，请重新开始"); }
                    self.flows.lock().await.remove(&callback.flow);
                    flow.cancelled.store(true,Ordering::SeqCst);
                    match normalized { Ok(account)=>proto::LoginPoll::Done {account},Err(_)=>failed("登录身份验证失败，请重新登录或导入 Token") }
                },
                _=>failed("登录操作无效"),
            }
        }
    }
}
