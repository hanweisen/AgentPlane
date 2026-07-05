use std::process::ExitCode;

use anyhow::Result;
use reqwest::StatusCode;

use crate::cli_client::{post_json, print_error_response};
use crate::config::ClientProfile;
use crate::protocol::{
    AgentMode, LeaseReleaseRequest, LeaseRenewRequest, ModeGetRequest, ModeSwitchRequest,
};

use super::{AgentModeArg, LeaseReleaseArgs, LeaseRenewArgs, ModeGetArgs, ModeSwitchArgs};

pub(super) async fn mode_get(args: ModeGetArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let response = post_json(&auth, "/v1/mode/get", &ModeGetRequest {}, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn mode_switch(args: ModeSwitchArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = ModeSwitchRequest {
        mode: match args.mode {
            AgentModeArg::Single => AgentMode::Single,
            AgentModeArg::Shared => AgentMode::Shared,
        },
        task_id: args.task_id,
        lease_id: args.lease_id,
        ttl_seconds: args.ttl_seconds,
        heartbeat_seconds: args.heartbeat_seconds,
        max_renewals: args.max_renewals,
    };
    let response = post_json(&auth, "/v1/mode/switch", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn lease_renew(args: LeaseRenewArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = LeaseRenewRequest {
        task_id: args.task_id,
        lease_id: args.lease_id,
    };
    let response = post_json(&auth, "/v1/lease/renew", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn lease_release(
    args: LeaseReleaseArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = LeaseReleaseRequest {
        task_id: args.task_id,
        lease_id: args.lease_id,
    };
    let response = post_json(&auth, "/v1/lease/release", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}
