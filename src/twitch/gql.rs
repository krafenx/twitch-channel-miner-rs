use crate::constants::{gql, CLIENT_ID, CLIENT_VERSION, GQL_URL, TWITCH_URL, USER_AGENT};
use crate::error::{MinerError, Result};
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct GqlHeaders<'a> {
    pub auth_token: &'a str,
    pub device_id: &'a str,
    pub client_version: &'a str,
    pub client_session_id: Option<&'a str>,
}

impl<'a> GqlHeaders<'a> {
    pub fn basic(auth_token: &'a str, device_id: &'a str) -> Self {
        Self {
            auth_token,
            device_id,
            client_version: CLIENT_VERSION,
            client_session_id: None,
        }
    }
}

async fn send_gql_single(
    client: &Client,
    headers: &GqlHeaders<'_>,
    op_name: &str,
    sha256_hash: &str,
    variables: &Value,
) -> Result<Value> {
    let body = json!({
        "operationName": op_name,
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": sha256_hash
            }
        },
        "variables": variables
    });

    debug!("GQL {} vars={}", op_name, variables);

    let mut req = client
        .post(GQL_URL)
        .header("Client-ID", CLIENT_ID)
        .header("Client-Version", headers.client_version)
        .header("X-Device-Id", headers.device_id)
        .header("Authorization", format!("OAuth {}", headers.auth_token))
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json");

    if let Some(session_id) = headers.client_session_id {
        req = req.header("Client-Session-Id", session_id);
    }

    let resp = req
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;

    Ok(resp)
}

pub async fn gql_request_with_headers(
    client: &Client,
    headers: &GqlHeaders<'_>,
    operation: &gql::Op,
    variables: Value,
) -> Result<Value> {
    let resp = send_gql_single(client, headers, operation.name, operation.hash, &variables).await?;

    if let Some(errors) = resp.get("errors") {
        let err_str = format!("{errors:?}");
        if err_str.contains("PersistedQueryNotFound") {
            if let Some(fallback_hash) = operation.fallback_hash {
                debug!(
                    "GQL {} retrying with fallback persistedQuery hash",
                    operation.name
                );
                let retry_resp =
                    send_gql_single(client, headers, operation.name, fallback_hash, &variables)
                        .await?;
                if let Some(retry_errors) = retry_resp.get("errors") {
                    return Err(MinerError::Gql(format!("{retry_errors:?}")));
                }
                return Ok(retry_resp);
            }
        }
        return Err(MinerError::Gql(err_str));
    }

    Ok(resp)
}

pub async fn gql_batch_request(
    client: &Client,
    headers: &GqlHeaders<'_>,
    payload: &Value,
) -> Result<Value> {
    let mut req = client
        .post(GQL_URL)
        .header("Client-ID", CLIENT_ID)
        .header("Client-Version", headers.client_version)
        .header("X-Device-Id", headers.device_id)
        .header("Authorization", format!("OAuth {}", headers.auth_token))
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json");

    if let Some(session_id) = headers.client_session_id {
        req = req.header("Client-Session-Id", session_id);
    }

    let resp = req
        .json(payload)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp)
}

pub async fn fetch_twilight_client_version(client: &Client) -> Option<String> {
    let page = client
        .get(TWITCH_URL)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let re = Regex::new(
        r#"window\.__twilightBuildID\s*=\s*"([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})""#,
    )
    .ok()?;
    re.captures(&page)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub async fn gql_request(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    operation: &gql::Op,
    variables: Value,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    gql_request_with_headers(client, &headers, operation, variables).await
}

pub async fn get_channel_id_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    login: &str,
) -> Result<String> {
    let resp =
        gql_request_with_headers(client, headers, &gql::GET_ID_FROM_LOGIN, json!({ "login": login }))
            .await?;

    resp["data"]["user"]["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| MinerError::StreamerNotFound(login.to_string()))
}

pub async fn get_channel_id(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    login: &str,
) -> Result<String> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_channel_id_ext(client, &headers, login).await
}

pub async fn get_channel_points_context_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_login: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::CHANNEL_POINTS_CONTEXT,
        json!({ "channelLogin": channel_login }),
    )
    .await
}

pub async fn get_channel_points_context(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_login: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_channel_points_context_ext(client, &headers, channel_login).await
}

pub async fn claim_bonus_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_id: &str,
    claim_id: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::CLAIM_COMMUNITY_POINTS,
        json!({
            "input": {
                "channelID": channel_id,
                "claimID": claim_id
            }
        }),
    )
    .await
}

pub async fn claim_bonus(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_id: &str,
    claim_id: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    claim_bonus_ext(client, &headers, channel_id, claim_id).await
}

pub async fn is_stream_live_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_id: &str,
) -> Result<bool> {
    let resp = gql_request_with_headers(
        client,
        headers,
        &gql::WITH_IS_STREAM_LIVE,
        json!({ "id": channel_id }),
    )
    .await?;

    Ok(resp["data"]["user"]["stream"].is_object())
}

pub async fn is_stream_live(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_id: &str,
) -> Result<bool> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    is_stream_live_ext(client, &headers, channel_id).await
}

pub async fn get_stream_info_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_login: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::VIDEO_PLAYER_STREAM_INFO,
        json!({ "channel": channel_login }),
    )
    .await
}

pub async fn get_stream_info(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_login: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_stream_info_ext(client, &headers, channel_login).await
}

pub async fn get_playback_access_token_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    login: &str,
) -> Result<(String, String)> {
    let resp = match send_gql_single(
        client,
        headers,
        gql::PLAYBACK_ACCESS_TOKEN.name,
        gql::PLAYBACK_ACCESS_TOKEN.hash,
        &json!({
            "login": login,
            "isLive": true,
            "isVod": false,
            "vodID": "",
            "playerType": "site",
            "platform": "web"
        }),
    )
    .await
    {
        Ok(val) if val.get("errors").is_none() => val,
        _ => {
            let fb = gql::PLAYBACK_ACCESS_TOKEN
                .fallback_hash
                .unwrap_or(gql::PLAYBACK_ACCESS_TOKEN.hash);
            let retry = send_gql_single(
                client,
                headers,
                gql::PLAYBACK_ACCESS_TOKEN.name,
                fb,
                &json!({
                    "login": login,
                    "isLive": true,
                    "isVod": false,
                    "vodID": "",
                    "playerType": "site"
                }),
            )
            .await?;
            if let Some(errs) = retry.get("errors") {
                return Err(MinerError::Gql(format!("{errs:?}")));
            }
            retry
        }
    };

    let token = &resp["data"]["streamPlaybackAccessToken"];
    let signature = token["signature"]
        .as_str()
        .ok_or_else(|| MinerError::Gql("missing signature".into()))?
        .to_string();
    let value = token["value"]
        .as_str()
        .ok_or_else(|| MinerError::Gql("missing value".into()))?
        .to_string();

    Ok((signature, value))
}

pub async fn get_playback_access_token(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    login: &str,
) -> Result<(String, String)> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_playback_access_token_ext(client, &headers, login).await
}

pub async fn get_inventory_ext(client: &Client, headers: &GqlHeaders<'_>) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::INVENTORY,
        json!({ "fetchRewardCampaigns": true }),
    )
    .await
}

pub async fn get_inventory(client: &Client, auth_token: &str, device_id: &str) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_inventory_ext(client, &headers).await
}

pub async fn claim_drop_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    drop_instance_id: &str,
) -> Result<bool> {
    let resp = gql_request_with_headers(
        client,
        headers,
        &gql::CLAIM_DROP_REWARDS,
        json!({
            "input": {
                "dropInstanceID": drop_instance_id
            }
        }),
    )
    .await?;

    if resp["data"]["claimDropRewards"].is_null() {
        return Ok(false);
    }
    if let Some(status) = resp["data"]["claimDropRewards"]["status"].as_str() {
        Ok(matches!(
            status,
            "ELIGIBLE_FOR_ALL" | "DROP_INSTANCE_ALREADY_CLAIMED"
        ))
    } else {
        Ok(false)
    }
}

pub async fn claim_drop(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    drop_instance_id: &str,
) -> Result<bool> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    claim_drop_ext(client, &headers, drop_instance_id).await
}

pub async fn get_drops_dashboard_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::VIEWER_DROPS_DASHBOARD,
        json!({ "fetchRewardCampaigns": true }),
    )
    .await
}

pub async fn get_drops_dashboard(
    client: &Client,
    auth_token: &str,
    device_id: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_drops_dashboard_ext(client, &headers).await
}

pub async fn get_campaign_details_batch(
    client: &Client,
    headers: &GqlHeaders<'_>,
    campaign_ids: &[String],
    user_id: &str,
) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for chunk in campaign_ids.chunks(20) {
        let batch: Vec<Value> = chunk
            .iter()
            .map(|cid| {
                json!({
                    "operationName": gql::DROP_CAMPAIGN_DETAILS.name,
                    "extensions": {
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": gql::DROP_CAMPAIGN_DETAILS.hash
                        }
                    },
                    "variables": {
                        "dropID": cid,
                        "channelLogin": user_id
                    }
                })
            })
            .collect();

        let resp = gql_batch_request(client, headers, &Value::Array(batch)).await?;
        if let Some(arr) = resp.as_array() {
            for item in arr {
                if let Some(dc) = item
                    .get("data")
                    .and_then(|d| d.get("user"))
                    .and_then(|u| u.get("dropCampaign"))
                {
                    if !dc.is_null() {
                        results.push(dc.clone());
                    }
                }
            }
        }
    }
    Ok(results)
}

pub async fn get_available_drops_for_channel(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_id: &str,
) -> Result<Vec<String>> {
    let resp = gql_request_with_headers(
        client,
        headers,
        &gql::AVAILABLE_DROPS,
        json!({ "channelID": channel_id }),
    )
    .await?;

    let ids = resp["data"]["channel"]["viewerDropCampaigns"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(ids)
}

pub async fn viewer_is_mod(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_login: &str,
) -> bool {
    match gql_request_with_headers(
        client,
        headers,
        &gql::MOD_VIEW_CHANNEL,
        json!({ "channelLogin": channel_login }),
    )
    .await
    {
        Ok(resp) => resp["data"]["user"]["self"]["isModerator"]
            .as_bool()
            .unwrap_or(false),
        Err(_) => false,
    }
}

pub async fn make_prediction_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    event_id: &str,
    outcome_id: &str,
    points: u64,
    transaction_id: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::MAKE_PREDICTION,
        json!({
            "input": {
                "eventID": event_id,
                "outcomeID": outcome_id,
                "points": points,
                "transactionID": transaction_id
            }
        }),
    )
    .await
}

pub async fn make_prediction(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    event_id: &str,
    outcome_id: &str,
    points: u64,
    transaction_id: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    make_prediction_ext(client, &headers, event_id, outcome_id, points, transaction_id).await
}

pub async fn get_followers(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    limit: u32,
    order: &str,
) -> Result<Vec<String>> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    get_followers_ext(client, &headers, limit, order).await
}

pub async fn get_followers_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    limit: u32,
    order: &str,
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut variables = json!({
            "limit": limit,
            "order": order
        });
        if let Some(c) = &cursor {
            variables["cursor"] = json!(c);
        }

        let resp =
            gql_request_with_headers(client, headers, &gql::CHANNEL_FOLLOWS, variables).await?;

        let follows = &resp["data"]["user"]["follows"];
        if let Some(edges) = follows["edges"].as_array() {
            for edge in edges {
                if let Some(login) = edge["node"]["login"].as_str() {
                    result.push(login.to_lowercase());
                }
                if let Some(c) = edge["cursor"].as_str() {
                    cursor = Some(c.to_string());
                }
            }
        }

        let has_next = follows["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false);
        if !has_next || edges_empty(&resp) {
            break;
        }
    }

    Ok(result)
}

fn edges_empty(resp: &Value) -> bool {
    resp["data"]["user"]["follows"]["edges"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true)
}

pub async fn join_raid_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    raid_id: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::JOIN_RAID,
        json!({
            "input": {
                "raidID": raid_id
            }
        }),
    )
    .await
}

pub async fn join_raid(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    raid_id: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    join_raid_ext(client, &headers, raid_id).await
}

pub async fn claim_moment_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    moment_id: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::COMMUNITY_MOMENT_CLAIM,
        json!({
            "input": {
                "momentID": moment_id
            }
        }),
    )
    .await
}

pub async fn claim_moment(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    moment_id: &str,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    claim_moment_ext(client, &headers, moment_id).await
}

pub async fn get_user_points_contribution_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    channel_login: &str,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::USER_POINTS_CONTRIBUTION,
        json!({ "channelLogin": channel_login }),
    )
    .await
}

pub async fn contribute_community_goal_ext(
    client: &Client,
    headers: &GqlHeaders<'_>,
    goal_id: &str,
    channel_id: &str,
    points: u64,
) -> Result<Value> {
    gql_request_with_headers(
        client,
        headers,
        &gql::CONTRIBUTE_COMMUNITY_GOAL,
        json!({
            "input": {
                "amount": points,
                "channelID": channel_id,
                "goalID": goal_id,
                "transactionID": uuid::Uuid::new_v4().to_string().replace('-', "")
            }
        }),
    )
    .await
}

pub async fn contribute_community_goal(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    goal_id: &str,
    channel_id: &str,
    points: u64,
) -> Result<Value> {
    let headers = GqlHeaders::basic(auth_token, device_id);
    contribute_community_goal_ext(client, &headers, goal_id, channel_id, points).await
}
