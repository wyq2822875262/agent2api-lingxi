mod account;
mod http;
mod login;
use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::{ChatPlan, PluginCtx, Provider, ProviderMeta, SendFuture, StoreAccount};
use serde_json::{json,Value};
use std::{collections::{HashMap,HashSet},sync::Arc};
use tokio::sync::Mutex;
const ID:&str="lingxi";
#[derive(Default)]
struct Lingxi { locks:Mutex<HashMap<String,Arc<Mutex<()>>>>, login:login::Manager }
impl Lingxi {
    async fn lock(&self,id:&str)->Arc<Mutex<()>> { self.locks.lock().await.entry(id.into()).or_default().clone() }
    async fn stored(&self,ctx:&PluginCtx,id:&str)->Result<StoreAccount,String> {
        if id.is_empty() { ctx.store_list(ID).await?.into_iter().next().ok_or_else(||"请先添加移动灵犀账户".into()) }
        else { let stored=ctx.store_get(ID,id).await?; if stored.provider!=ID {return Err("账户提供商不匹配".into());} Ok(stored) }
    }
    async fn refresh_locked(&self,ctx:&PluginCtx,stored:StoreAccount,proxy:&str)->Result<StoreAccount,String> {
        let refresh=account::text(&stored.secret,"refresh_token");
        if refresh.is_empty() {return Err(agent2api_provider_sdk::control_error(401,"此账户未提供 Refresh Token，请重新短信登录或导入凭证",None,None));}
        let data=http::post(proxy,http::REFRESH,Some(account::text(&stored.secret,"access_token")),&json!({"refresh_token":refresh})).await.map_err(|e|e.message())?;
        let mut candidate=data;
        if account::text(&candidate,"refresh_token").is_empty() {candidate["refresh_token"]=json!(refresh);}
        let mut secret=account::secret(&candidate)?;
        let user=http::post(proxy,http::USER,Some(account::text(&secret,"access_token")),&json!({})).await.map_err(|e|e.message())?;
        secret=account::bind(secret,&user,Some(account::text(&stored.secret,"identity")))?;
        secret["name"]=stored.secret.get("name").cloned().unwrap_or(json!(""));
        if ctx.store_update(&stored.id,&stored.secret,&secret).await? {
            ctx.notify_auth_changed(ID,&stored.id).await;
            Ok(StoreAccount {secret:secret.clone(),public:account::public(&secret),..stored})
        } else {
            let current=self.stored(ctx,&stored.id).await?;
            if current.secret==stored.secret {return Err("凭证更新未保存，请稍后重试".into());}
            if account::text(&current.secret,"identity")!=account::text(&stored.secret,"identity") {return Err("账户已被替换，请重新选择账户".into());}
            Ok(current)
        }
    }
    fn expiring(secret:&Value)->bool { secret.get("access_token_expires_at").and_then(account::expires).is_some_and(|v|v<=http::now_ms()/1000+90) }
    async fn ready(&self,ctx:&PluginCtx,id:&str,explicit:Option<&proto::WireProxy>,force:bool)->Result<(StoreAccount,String),String> {
        let first=self.stored(ctx,id).await?;
        let lock=self.lock(&first.id).await; let _guard=lock.lock().await;
        let mut stored=self.stored(ctx,&first.id).await?;
        let proxy=http::resolve_proxy(ctx,&stored,explicit)?;
        let rotated=account::text(&first.secret,"access_token")!=account::text(&stored.secret,"access_token");
        if (force && !rotated) || (Self::expiring(&stored.secret) && !account::text(&stored.secret,"refresh_token").is_empty()) {stored=self.refresh_locked(ctx,stored,&proxy).await?;}
        account::token(account::text(&stored.secret,"access_token"))?;
        Ok((stored,proxy))
    }
    async fn business(&self,ctx:&PluginCtx,id:&str,path:&str,payload:&Value)->Result<Value,String> {
        let (stored,proxy)=self.ready(ctx,id,None,false).await?;
        match http::post(&proxy,path,Some(account::text(&stored.secret,"access_token")),payload).await {
            Ok(data)=>Ok(data),
            Err(error) if error.auth()=>{
                let lock=self.lock(&stored.id).await;let _guard=lock.lock().await;
                let mut current=self.stored(ctx,&stored.id).await?;
                let proxy=http::resolve_proxy(ctx,&current,None)?;
                if account::text(&current.secret,"access_token")==account::text(&stored.secret,"access_token") {current=self.refresh_locked(ctx,current,&proxy).await?;}
                http::post(&proxy,path,Some(account::text(&current.secret,"access_token")),payload).await.map_err(|e|e.message())
            },
            Err(error)=>Err(error.message())
        }
    }
    async fn catalog(&self,ctx:&PluginCtx,id:&str)->Result<proto::ModelList,String> {
        let data=self.business(ctx,id,http::MODELS,&json!({})).await?;
        let rows=data.as_array().or_else(||data.get("items").and_then(Value::as_array)).ok_or("移动灵犀模型目录格式无效")?;
        let mut models=Vec::new();let mut seen=HashSet::new();
        for row in rows {
            let id=account::text(row,"modelName");
            if id.is_empty() {return Err("移动灵犀模型缺少 modelName，无法确定请求编码".into());}
            if !seen.insert(id.to_string()) {continue;}
            let mut item=row.clone();
            item["upstream_id"]=row.get("id").cloned().unwrap_or(Value::Null);
            item["id"]=json!(id);item["object"]=json!("model");item["owned_by"]=json!(ID);
            if account::text(row,"name").is_empty() {item["name"]=json!(id);}
            for (source,target) in [("context_length","contextWindow"),("max_output_tokens","maxOutputTokens")] {
                if let Some(v)=row.get(source).or_else(||row["extInfo"].get(source)).filter(|v|v.is_number()) {item[target]=v.clone();}
            }
            models.push(item);
        }
        if models.is_empty(){return Err("移动灵犀返回空模型目录，请检查账户权限".into());}
        models.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(proto::ModelList {models,meta:proto::CatalogMeta {remote:true,refreshed_at:http::now_ms()}})
    }
    async fn daily(&self,ctx:&PluginCtx,id:&str)->Result<proto::CheckinReport,String> {
        let stored=self.stored(ctx,id).await?;
        // Separate from token locks because business() may need a credential refresh.
        let lock=self.lock(&format!("claim:{}",stored.id)).await;let _guard=lock.lock().await;
        let data=self.business(ctx,&stored.id,http::CAMPAIGNS,&json!({})).await?;
        let rows=data.get("items").and_then(Value::as_array).ok_or("移动灵犀活动列表格式无效")?;
        let day=(chrono::Utc::now()+chrono::Duration::hours(8)).format("%Y-%m-%d").to_string();
        let mut ledger=ctx.ledger_get(&stored.id,"lingxi-daily").await?;
        if ledger["day"]!=day {ledger=json!({"day":day,"campaigns":{}});}
        let mut claimed=false;let mut done=false;let mut uncertain=false;
        for row in rows.iter().filter(|r|account::text(r,"activity_name")=="AI每日畅想计划" || account::text(r,"activity_title")=="AI每日畅想计划") {
            let campaign=row.get("campaign_id").filter(|v|v.is_string()||v.is_number()).cloned().ok_or("移动灵犀活动缺少 campaign_id")?;
            let key=campaign.as_str().map(str::to_owned).unwrap_or_else(||campaign.to_string());
            if key.is_empty() || key.len()>100 {continue;}
            let prior=ledger["campaigns"].get(&key).and_then(Value::as_str).unwrap_or("");
            if account::text(row,"claim_status")=="SUCCESS" || ["SUCCESS","ALREADY_CLAIMED"].contains(&prior) {done=true;continue;}
            if account::text(row,"claim_status")=="PROCESSING" || ["PROCESSING","UNKNOWN"].contains(&prior) {uncertain=true;continue;}
            if row["claimable"]!=true || row["status"].as_i64()!=Some(1) || row["eligible"]==false || row["is_eligible"]==false {continue;}
            ledger["campaigns"][&key]=json!("PROCESSING");
            ctx.ledger_put(&stored.id,"lingxi-daily",&ledger).await?;
            // Do not replay a write after auth/network failure. Query state on the next run.
            let (current,proxy)=self.ready(ctx,&stored.id,None,false).await?;
            let result=http::post(&proxy,&format!("{}/claim",http::CAMPAIGNS),Some(account::text(&current.secret,"access_token")),&json!({"campaign_id":campaign})).await;
            let status=match result {
                Ok(reply) if account::text(&reply,"status")=="SUCCESS"=>{
                    if account::text(&reply,"message").contains("已领取") {done=true;"ALREADY_CLAIMED"} else {claimed=true;"SUCCESS"}
                },
                Ok(reply) if account::text(&reply,"status")=="FAILED"=>{uncertain=true;"FAILED"},
                _=>{uncertain=true;"UNKNOWN"}
            };
            ledger["campaigns"][&key]=json!(status);ctx.ledger_put(&stored.id,"lingxi-daily",&ledger).await?;
        }
        let (outcome,message)=if uncertain {(proto::CheckinOutcome::Failed,"部分活动处理结果待确认，请查看移动灵犀活动状态；不会自动重放未知领取")}
            else if claimed {(proto::CheckinOutcome::Claimed,"移动灵犀每日活动领取成功，奖励以余额查询为准")}
            else if done {(proto::CheckinOutcome::Done,"今日活动已领取")}
            else {(proto::CheckinOutcome::Inactive,"当前没有符合条件的每日活动")};
        Ok(proto::CheckinReport {outcome,credit:None,streak:None,message:Some(message.into())})
    }
}
impl Provider for Lingxi {
    fn meta(&self)->ProviderMeta {
        ProviderMeta::new(ID,"移动灵犀").with_capabilities(proto::Capabilities {refresh:true,model_refresh:true,usage:true,checkin:true,web_login:true,sse_model_rewrite:true,..Default::default()})
            .with_login(vec![proto::LoginMethod::Token {label:"Access Token".into(),placeholder:"移动灵犀官网 access_token".into(),extra_fields:vec![proto::LoginField {key:"refresh_token".into(),label:"Refresh Token".into(),placeholder:"可选，用于续期".into(),optional:true,multiline:false}]}])
            .with_account_features(proto::AccountFeatures {usage:true,identifier:vec!["phone".into()],expiry:vec!["expiresAt".into()],concurrency_default:Some(6),..Default::default()})
            .with_checkin(proto::CheckinInfo {description:Some("查询并领取移动灵犀「AI每日畅想计划」，资格与奖励以上游为准".into()),..Default::default()})
    }
    fn normalize_account<'a>(&'a self,ctx:&'a PluginCtx,input:proto::NormalizeInput)->SendFuture<'a,proto::NormalizedAccount> {Box::pin(async move {
        use base64::Engine;
        let payload=match input.source {
            proto::NormalizeSource::Manual {payload}=>payload,
            proto::NormalizeSource::FileText {content,..}=>account::file(&content)?,
            proto::NormalizeSource::FileBase64 {content,..}=>{let bytes=base64::engine::general_purpose::STANDARD.decode(content.trim()).map_err(|_|"账号文件不是有效 base64")?;account::file(std::str::from_utf8(&bytes).map_err(|_|"账号文件必须为 UTF-8")?)?},
            proto::NormalizeSource::Desktop=>return Err("UNSUPPORTED:请使用短信登录或导入移动灵犀 Token".into())
        };
        let payload=account::manual(payload)?;
        let mut secret=account::secret(&payload)?;
        let proxy=input.proxy.as_ref().map(|p|p.to_url()).unwrap_or(http::login_proxy(ctx,None)?);
        let user=http::post(&proxy,http::USER,Some(account::text(&secret,"access_token")),&json!({})).await.map_err(|e|e.message())?;
        secret=account::bind(secret,&user,None)?;
        account::normalized(secret,input.name.as_deref().or_else(||payload.get("name").and_then(Value::as_str)))
    })}
    fn account_view(&self,secret:&Value)->Result<Value,String>{Ok(account::public(secret))}
    fn account_identity(&self,secret:&Value)->Result<String,String>{let id=account::text(secret,"identity");if id.is_empty(){Err("账户缺少已验证身份".into())}else{Ok(format!("lingxi:{id}"))}}
    fn admin_action<'a>(&'a self,_ctx:&'a PluginCtx,_id:&'a str,action:&'a str,_payload:&'a Value)->SendFuture<'a,Value>{Box::pin(async move {if action=="auth.methods"{Ok(account::methods())}else{Err(agent2api_provider_sdk::unsupported("移动灵犀不支持此管理操作"))}})}
    fn ensure_token<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,String>{Box::pin(async move {Ok(account::text(&self.ready(ctx,id,None,false).await?.0.secret,"access_token").into())})}
    fn refresh_token<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,String>{Box::pin(async move {Ok(account::text(&self.ready(ctx,id,None,true).await?.0.secret,"access_token").into())})}
    fn token_expiring<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,bool>{Box::pin(async move {let stored=self.stored(ctx,id).await?;Ok(Self::expiring(&stored.secret)&&!account::text(&stored.secret,"refresh_token").is_empty())})}
    fn build_chat<'a>(&'a self,ctx:&'a PluginCtx,req:proto::ChatRequest)->SendFuture<'a,ChatPlan>{Box::pin(async move {
        let (stored,_)=self.ready(ctx,&req.account,req.proxy.as_ref(),false).await?;
        let mut body=req.body;let object=body.as_object_mut().ok_or("对话请求必须为 JSON 对象")?;
        let model=if !req.wire_model.is_empty(){req.wire_model.as_str()}else{object.get("model").and_then(Value::as_str).unwrap_or("")};
        if model.is_empty(){return Err("请先刷新移动灵犀模型目录并选择模型".into());}
        let model=model.to_string();object.insert("model".into(),json!(model));object.insert("stream".into(),json!(req.stream));
        Ok(ChatPlan::chat(format!("{}{}",http::base()?,http::CHAT),vec![("Authorization".into(),format!("Bearer {}",account::text(&stored.secret,"access_token"))),("X-Client-Id".into(),"lingxiclaw".into()),("Content-Type".into(),"application/json".into()),("Accept".into(),if req.stream{"text/event-stream"}else{"application/json"}.into())],body))
    })}
    fn models<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,proto::ModelList>{Box::pin(async move {self.catalog(ctx,id).await})}
    fn refresh_models<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str,_force:bool)->SendFuture<'a,proto::RefreshOutcome>{Box::pin(async move {Ok(proto::RefreshOutcome::refreshed(self.catalog(ctx,id).await?.models.len()))})}
    fn usage<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,Value>{Box::pin(async move {
        let data=self.business(ctx,id,http::BALANCE,&json!({})).await?;
        let n=data.get("compute_credit").and_then(|v|v.as_f64().or_else(||v.as_str()?.parse().ok())).filter(|v|v.is_finite()).ok_or("移动灵犀余额缺少有效 compute_credit")?;
        Ok(json!({"provider":ID,"plan":"移动灵犀算力豆","balance":n,"unit":"算力豆","windows":[{"name":"算力豆","label":"算力豆","remaining":n,"balance":n,"unit":"算力豆","display":n.to_string()}]}))
    })}
    fn checkin<'a>(&'a self,ctx:&'a PluginCtx,id:&'a str)->SendFuture<'a,proto::CheckinReport>{Box::pin(async move {self.daily(ctx,id).await})}
    fn login_start<'a>(&'a self,ctx:&'a PluginCtx,_method:&'a str,payload:&'a Value)->SendFuture<'a,proto::LoginStart>{Box::pin(async move {self.login.start(ctx,payload).await})}
    fn login_poll<'a>(&'a self,_ctx:&'a PluginCtx,flow:&'a str)->SendFuture<'a,proto::LoginPoll>{Box::pin(async move {Ok(self.login.poll(flow).await)})}
    fn login_cancel<'a>(&'a self,_ctx:&'a PluginCtx,flow:&'a str)->SendFuture<'a,()>{Box::pin(async move {self.login.cancel(flow).await;Ok(())})}
    fn login_callback<'a>(&'a self,_ctx:&'a PluginCtx,callback:proto::LoginCallback)->SendFuture<'a,proto::LoginPoll>{Box::pin(async move {Ok(self.login.callback(callback).await)})}
    fn classify(&self,status:u16,body:&Value)->proto::ErrorClass {
        let value=body.get("code").or_else(||body.pointer("/error/code"));let code=value.and_then(|v|v.as_i64().or_else(||v.as_str()?.parse().ok()));
        if status==401 || [Some(3004),Some(1017),Some(1018)].contains(&code) {proto::ErrorClass::TokenExpired {message:"移动灵犀登录已失效".into()}}
        else if status==402||status==429{proto::ErrorClass::QuotaLimited {reset_at:None,message:"移动灵犀额度不足或请求频繁".into(),upstream_code:code,status:429}}
        else {proto::ErrorClass::Fatal {status:if status<400{502}else{status},message:"移动灵犀上游请求失败".into(),upstream_code:code}}
    }
    fn is_error_response(&self,status:u16,_headers:&std::collections::BTreeMap<String,String>)->bool{status>=400}
}
fn main(){agent2api_provider_sdk::serve_blocking(vec![Box::new(Lingxi::default())]);}
