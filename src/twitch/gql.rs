use crate::constants::{gql, CLIENT_ID, CLIENT_VERSION, GQL_URL, USER_AGENT};
use crate::error::{MinerError, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

pub async fn gql_request(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    operation: &gql::Op,
    variables: Value,
) -> Result<Value> {
    let body = json!({
        "operationName": operation.name,
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": operation.hash
            }
        },
        "variables": variables
    });

    debug!("GQL {} vars={}", operation.name, variables);

    let resp = client
        .post(GQL_URL)
        .header("Client-ID", CLIENT_ID)
        .header("Client-Version", CLIENT_VERSION)
        .header("X-Device-Id", device_id)
        .header("Authorization", format!("OAuth {}", auth_token))
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;

    if let Some(errors) = resp.get("errors") {
        return Err(MinerError::Gql(format!("{:?}", errors)));
    }

    Ok(resp)
}

pub async fn get_channel_id(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    login: &str,
) -> Result<String> {
    let resp = gql_request(
        client,
        auth_token,
        device_id,
        &gql::GET_ID_FROM_LOGIN,
        json!({ "login": login }),
    )
    .await?;

    resp["data"]["user"]["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| MinerError::StreamerNotFound(login.to_string()))
}

pub async fn get_channel_points_context(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_login: &str,
) -> Result<Value> {
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::CHANNEL_POINTS_CONTEXT,
        json!({ "channelLogin": channel_login }),
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
    gql_request(
        client,
        auth_token,
        device_id,
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

pub async fn is_stream_live(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_id: &str,
) -> Result<bool> {
    let resp = gql_request(
        client,
        auth_token,
        device_id,
        &gql::WITH_IS_STREAM_LIVE,
        json!({ "id": channel_id }),
    )
    .await?;

    Ok(resp["data"]["user"]["stream"].is_object())
}

pub async fn get_stream_info(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    channel_login: &str,
) -> Result<Value> {
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::VIDEO_PLAYER_STREAM_INFO,
        json!({ "channel": channel_login }),
    )
    .await
}

pub async fn get_playback_access_token(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    login: &str,
) -> Result<(String, String)> {
    let resp = gql_request(
        client,
        auth_token,
        device_id,
        &gql::PLAYBACK_ACCESS_TOKEN,
        json!({
            "login": login,
            "isLive": true,
            "isVod": false,
            "vodID": "",
            "playerType": "site"
        }),
    )
    .await?;

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

pub async fn get_inventory(client: &Client, auth_token: &str, device_id: &str) -> Result<Value> {
    gql_request(client, auth_token, device_id, &gql::INVENTORY, json!({})).await
}

pub async fn claim_drop(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    drop_instance_id: &str,
) -> Result<bool> {
    let resp = gql_request(
        client,
        auth_token,
        device_id,
        &gql::CLAIM_DROP_REWARDS,
        json!({
            "input": {
                "dropInstanceID": drop_instance_id
            }
        }),
    )
    .await?;

    if let Some(status) = resp["data"]["claimDropRewards"]["status"].as_str() {
        Ok(matches!(
            status,
            "ELIGIBLE_FOR_ALL" | "DROP_INSTANCE_ALREADY_CLAIMED"
        ))
    } else {
        Ok(false)
    }
}

pub async fn get_drops_dashboard(
    client: &Client,
    auth_token: &str,
    device_id: &str,
) -> Result<Value> {
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::VIEWER_DROPS_DASHBOARD,
        json!({ "fetchRewardCampaigns": true }),
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
    gql_request(
        client,
        auth_token,
        device_id,
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

pub async fn get_followers(
    client: &Client,
    auth_token: &str,
    device_id: &str,
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

        let resp = gql_request(
            client,
            auth_token,
            device_id,
            &gql::CHANNEL_FOLLOWS,
            variables,
        )
        .await?;

        let follows = &resp["data"]["user"]["follows"];
        if let Some(edges) = follows["edges"].as_array() {
            for edge in edges {
                if let Some(login) = edge["node"]["login"].as_str() {
                    result.push(login.to_string());
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

pub async fn join_raid(
    client: &Client,
    auth_token: &str,
    device_id: &str,
    raid_id: &str,
) -> Result<Value> {
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::JOIN_RAID,
        json!({
            "input": {
                "raidID": raid_id
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
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::COMMUNITY_MOMENT_CLAIM,
        json!({
            "input": {
                "momentID": moment_id
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
    gql_request(
        client,
        auth_token,
        device_id,
        &gql::CONTRIBUTE_COMMUNITY_GOAL,
        json!({
            "input": {
                "communityPointsContribution": {
                    "amount": points,
                    "channelID": channel_id,
                    "goalID": goal_id,
                    "transactionID": uuid::Uuid::new_v4().to_string().replace("-", "")
                }
            }
        }),
    )
    .await
}
