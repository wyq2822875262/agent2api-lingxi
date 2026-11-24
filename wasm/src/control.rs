use super::{account, ready, upstream, ID};
use agent2api_wasm_sdk::{self as sdk, catalog, json, Error, Result, Value};
use std::collections::HashSet;

pub fn models() -> Result<Value> {
    let data = super::business(upstream::MODELS)?;
    let rows = data
        .as_array()
        .or_else(|| data.get("items").and_then(Value::as_array))
        .ok_or_else(|| Error::http(502, "移动灵犀模型目录格式无效"))?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let id = account::text(row, "modelName");
        if id.is_empty() {
            return Err(Error::http(
                502,
                "移动灵犀模型缺少 modelName，无法确定请求编码",
            ));
        }
        if !seen.insert(id.to_string()) {
            continue;
        }
        let mut item = row.clone();
        item["upstream_id"] = row.get("id").cloned().unwrap_or(Value::Null);
        item["id"] = json!(id);
        item["object"] = json!("model");
        item["owned_by"] = json!(ID);
        if account::text(row, "name").is_empty() {
            item["name"] = json!(id);
        }
        for (source, target) in [
            ("context_length", "contextWindow"),
            ("max_output_tokens", "maxOutputTokens"),
        ] {
            if let Some(v) = row
                .get(source)
                .or_else(|| row["extInfo"].get(source))
                .filter(|v| v.is_number())
            {
                item[target] = v.clone();
            }
        }
        models.push(item);
    }
    if models.is_empty() {
        return Err(Error::http(502, "移动灵犀返回空模型目录，请检查账户权限"));
    }
    models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(catalog::refreshed(models))
}

fn day() -> Result<String> {
    let now = sdk::now();
    let date = chrono::DateTime::from_timestamp_millis(now.saturating_add(8 * 3600 * 1000))
        .filter(|_| now > 0)
        .ok_or_else(|| Error::new("宿主时钟不可用"))?;
    Ok(date.format("%Y-%m-%d").to_string())
}

pub fn daily() -> Result<Value> {
    // Account locks serialize the ledger read, claim and write. Avoid recursively locking refresh.
    let _guard = sdk::AccountLock::acquire()?;
    let token = ready(false)?;
    let data = match upstream::post(upstream::CAMPAIGNS, Some(&token), &json!({})) {
        Err(error) if error.status == Some(401) => {
            let token = ready(true)?;
            let data = upstream::post(upstream::CAMPAIGNS, Some(&token), &json!({}))?;
            (token, data)
        }
        reply => (token, reply?),
    };
    let (token, data) = data;
    let rows = data
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::http(502, "移动灵犀活动列表格式无效"))?;
    let day = day()?;
    let mut ledger = sdk::host("ledger.get", json!({"kind":"lingxi-daily"}))?;
    if ledger["day"] != day {
        ledger = json!({"day":day,"campaigns":{}});
    }
    let mut claimed = false;
    let mut done = false;
    let mut uncertain = false;
    for row in rows.iter().filter(|r| {
        account::text(r, "activity_name") == "AI每日畅想计划"
            || account::text(r, "activity_title") == "AI每日畅想计划"
    }) {
        let campaign = row
            .get("campaign_id")
            .filter(|v| v.is_string() || v.is_number())
            .cloned()
            .ok_or_else(|| Error::http(502, "移动灵犀活动缺少 campaign_id"))?;
        let key = campaign
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| campaign.to_string());
        if key.is_empty() || key.len() > 100 {
            continue;
        }
        let prior = ledger["campaigns"]
            .get(&key)
            .and_then(Value::as_str)
            .unwrap_or("");
        if account::text(row, "claim_status") == "SUCCESS"
            || ["SUCCESS", "ALREADY_CLAIMED"].contains(&prior)
        {
            done = true;
            continue;
        }
        if account::text(row, "claim_status") == "PROCESSING"
            || ["PROCESSING", "UNKNOWN"].contains(&prior)
        {
            uncertain = true;
            continue;
        }
        if row["claimable"] != true
            || row["status"].as_i64() != Some(1)
            || row["eligible"] == false
            || row["is_eligible"] == false
        {
            continue;
        }
        ledger["campaigns"][&key] = json!("PROCESSING");
        sdk::host("ledger.put", json!({"kind":"lingxi-daily","value":ledger}))?;
        // Do not replay a write after auth/network failure. Query state on the next run.
        let result = upstream::post(
            &format!("{}/claim", upstream::CAMPAIGNS),
            Some(&token),
            &json!({"campaign_id":campaign}),
        );
        let status = match result {
            Ok(reply) if account::text(&reply, "status") == "SUCCESS" => {
                if account::text(&reply, "message").contains("已领取") {
                    done = true;
                    "ALREADY_CLAIMED"
                } else {
                    claimed = true;
                    "SUCCESS"
                }
            }
            Ok(reply) if account::text(&reply, "status") == "FAILED" => {
                uncertain = true;
                "FAILED"
            }
            _ => {
                uncertain = true;
                "UNKNOWN"
            }
        };
        ledger["campaigns"][&key] = json!(status);
        sdk::host("ledger.put", json!({"kind":"lingxi-daily","value":ledger}))?;
    }
    let (outcome, message) = if uncertain {
        (
            "failed",
            "部分活动处理结果待确认，请查看移动灵犀活动状态；不会自动重放未知领取",
        )
    } else if claimed {
        ("claimed", "移动灵犀每日活动领取成功，奖励以余额查询为准")
    } else if done {
        ("done", "今日活动已领取")
    } else {
        ("inactive", "当前没有符合条件的每日活动")
    };
    Ok(json!({"outcome":outcome,"message":message}))
}
