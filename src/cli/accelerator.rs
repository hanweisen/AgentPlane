use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::StatusCode;

use crate::cli_client::{post_json, process_error_response};
use crate::config::{ClientProfile, ResolvedClientAuth};
use crate::protocol::{
    AcceleratorDevice, AcceleratorKind, AcceleratorProcess, AcceleratorStatusRequest,
    AcceleratorStatusResponse,
};

use super::{
    AcceleratorKindArg, AcceleratorPreflightArgs, AcceleratorStatusArgs, AcceleratorWaitIdleArgs,
    GpuPreflightArgs, GpuStatusArgs, GpuWaitIdleArgs, NpuStatusArgs,
};

struct AcceleratorReadinessPolicy {
    kind: AcceleratorKind,
    gpus: Option<String>,
    selected_gpus: Option<BTreeSet<u32>>,
    max_memory_mib: u64,
    max_util_percent: u32,
    forbid_match: Option<String>,
    forbid_regex: Option<Regex>,
}

#[derive(Debug, serde::Serialize)]
struct AcceleratorReadinessReport {
    ok: bool,
    passed: bool,
    kind: AcceleratorKind,
    available: bool,
    provider: Option<String>,
    gpus: Option<String>,
    max_memory_mib: u64,
    max_util_percent: u32,
    forbid_match: Option<String>,
    stable_seconds: Option<u64>,
    observed_stable_seconds: Option<u64>,
    timed_out: bool,
    blockers: Vec<AcceleratorReadinessBlocker>,
    snapshot: AcceleratorStatusResponse,
    agent_hint: String,
}

#[derive(Debug, serde::Serialize)]
struct AcceleratorReadinessBlocker {
    kind: String,
    gpu_index: Option<u32>,
    message: String,
    memory_used_mib: Option<u64>,
    max_memory_mib: Option<u64>,
    utilization_percent: Option<u32>,
    max_util_percent: Option<u32>,
    pid: Option<i32>,
    process_command: Option<String>,
    process_used_memory_mib: Option<u64>,
    pattern: Option<String>,
}

pub(super) async fn accelerator_status(
    args: AcceleratorStatusArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = AcceleratorStatusRequest {
        kind: accelerator_kind_from_arg(args.kind),
        gpus: args.gpus,
        process_match: args.process_match,
    };
    let body = post_accelerator_status(&auth, &payload).await?;
    print_accelerator_status(&body, args.text && !args.json)?;
    Ok(ExitCode::SUCCESS)
}

pub(super) async fn gpu_status(args: GpuStatusArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = AcceleratorStatusRequest {
        kind: AcceleratorKind::Gpu,
        gpus: args.gpus,
        process_match: args.process_match,
    };
    let body = post_accelerator_status(&auth, &payload).await?;
    print_accelerator_status(&body, args.text && !args.json)?;
    Ok(ExitCode::SUCCESS)
}

pub(super) async fn npu_status(args: NpuStatusArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = AcceleratorStatusRequest {
        kind: AcceleratorKind::Npu,
        gpus: args.gpus,
        process_match: args.process_match,
    };
    let body = post_accelerator_status(&auth, &payload).await?;
    print_accelerator_status(&body, args.text && !args.json)?;
    Ok(ExitCode::SUCCESS)
}

pub(super) async fn accelerator_preflight(
    args: AcceleratorPreflightArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let policy = accelerator_readiness_policy(
        accelerator_kind_from_arg(args.kind),
        args.gpus,
        args.max_memory_mib,
        args.max_util_percent,
        args.forbid_match,
    )?;
    accelerator_preflight_with_policy(
        &auth,
        policy,
        args.text && !args.json,
        "accelerator-preflight",
    )
    .await
}

pub(super) async fn gpu_preflight(
    args: GpuPreflightArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let policy = accelerator_readiness_policy(
        AcceleratorKind::Gpu,
        args.gpus,
        args.max_memory_mib,
        args.max_util_percent,
        args.forbid_match,
    )?;
    accelerator_preflight_with_policy(&auth, policy, args.text && !args.json, "gpu-preflight").await
}

pub(super) async fn accelerator_wait_idle(
    args: AcceleratorWaitIdleArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let policy = accelerator_readiness_policy(
        accelerator_kind_from_arg(args.kind),
        args.gpus,
        args.max_memory_mib,
        args.max_util_percent,
        args.forbid_match,
    )?;
    accelerator_wait_idle_with_policy(
        &auth,
        policy,
        args.stable_seconds,
        args.timeout_seconds,
        args.poll_ms,
        args.text && !args.json,
        "accelerator-wait-idle",
    )
    .await
}

pub(super) async fn gpu_wait_idle(
    args: GpuWaitIdleArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let policy = accelerator_readiness_policy(
        AcceleratorKind::Gpu,
        args.gpus,
        args.max_memory_mib,
        args.max_util_percent,
        args.forbid_match,
    )?;
    accelerator_wait_idle_with_policy(
        &auth,
        policy,
        args.stable_seconds,
        args.timeout_seconds,
        args.poll_ms,
        args.text && !args.json,
        "gpu-wait-idle",
    )
    .await
}

async fn accelerator_preflight_with_policy(
    auth: &ResolvedClientAuth,
    policy: AcceleratorReadinessPolicy,
    text: bool,
    command_name: &str,
) -> Result<ExitCode> {
    let payload = AcceleratorStatusRequest {
        kind: policy.kind.clone(),
        gpus: policy.gpus.clone(),
        process_match: None,
    };
    let status = post_accelerator_status(auth, &payload).await?;
    let report = build_accelerator_readiness_report(&policy, status, None, false);
    print_accelerator_readiness_report(&report, text, command_name)?;
    Ok(report_exit_code(&report))
}

async fn accelerator_wait_idle_with_policy(
    auth: &ResolvedClientAuth,
    policy: AcceleratorReadinessPolicy,
    stable_seconds: u64,
    timeout_seconds: u64,
    poll_ms: u64,
    text: bool,
    command_name: &str,
) -> Result<ExitCode> {
    if stable_seconds == 0 {
        bail!("--stable-seconds must be greater than 0");
    }
    if timeout_seconds == 0 {
        bail!("--timeout-seconds must be greater than 0");
    }
    if poll_ms == 0 {
        bail!("--poll-ms must be greater than 0");
    }

    let payload = AcceleratorStatusRequest {
        kind: policy.kind.clone(),
        gpus: policy.gpus.clone(),
        process_match: None,
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let stable_duration = Duration::from_secs(stable_seconds);
    let mut stable_since = None;
    loop {
        let status = post_accelerator_status(auth, &payload).await?;
        let now = Instant::now();
        let observed_stable_seconds =
            stable_since.map(|since: Instant| now.saturating_duration_since(since).as_secs());
        let mut report =
            build_accelerator_readiness_report(&policy, status, observed_stable_seconds, false);
        report.stable_seconds = Some(stable_seconds);

        if report.passed {
            let since = stable_since.get_or_insert(now);
            let observed = now.saturating_duration_since(*since);
            report.observed_stable_seconds = Some(observed.as_secs());
            if observed >= stable_duration {
                print_accelerator_readiness_report(&report, text, command_name)?;
                return Ok(ExitCode::SUCCESS);
            }
        } else {
            stable_since = None;
            report.observed_stable_seconds = Some(0);
        }

        if now >= deadline {
            report.timed_out = true;
            report.ok = false;
            report.passed = false;
            print_accelerator_readiness_report(&report, text, command_name)?;
            return Ok(ExitCode::from(1));
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(poll_ms))).await;
    }
}

async fn post_accelerator_status(
    auth: &ResolvedClientAuth,
    payload: &AcceleratorStatusRequest,
) -> Result<AcceleratorStatusResponse> {
    let response = post_json(auth, "/v1/accelerator/status", payload, true).await?;
    if response.status() == StatusCode::OK {
        return Ok(response.json().await?);
    }
    Err(process_error_response(response).await)
}

fn accelerator_kind_from_arg(kind: AcceleratorKindArg) -> AcceleratorKind {
    match kind {
        AcceleratorKindArg::Gpu => AcceleratorKind::Gpu,
        AcceleratorKindArg::Npu => AcceleratorKind::Npu,
    }
}

fn print_accelerator_status(body: &AcceleratorStatusResponse, text: bool) -> Result<()> {
    if !text {
        println!("{}", serde_json::to_string_pretty(body)?);
        return Ok(());
    }
    if !body.available {
        println!(
            "No {} accelerator detected: {}",
            accelerator_kind_label(&body.kind),
            body.reason.as_deref().unwrap_or("unavailable")
        );
        println!("{}", body.agent_hint);
        return Ok(());
    }
    println!(
        "{} accelerator provider: {}",
        accelerator_kind_label(&body.kind),
        body.provider.as_deref().unwrap_or("unknown")
    );
    for device in &body.devices {
        print_accelerator_device(&body.kind, device);
    }
    if body.processes.is_empty() {
        println!("No accelerator compute processes reported.");
    } else {
        println!("Processes:");
        for process in &body.processes {
            print_accelerator_process(&body.kind, process);
        }
    }
    Ok(())
}

fn accelerator_readiness_policy(
    kind: AcceleratorKind,
    gpus: Option<String>,
    max_memory_mib: u64,
    max_util_percent: u32,
    forbid_match: Option<String>,
) -> Result<AcceleratorReadinessPolicy> {
    let selected_gpus = gpus.as_deref().map(parse_gpu_selection_arg).transpose()?;
    let forbid_regex = forbid_match
        .as_deref()
        .map(|pattern| {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .with_context(|| format!("invalid --forbid-match regex: {pattern}"))
        })
        .transpose()?;
    Ok(AcceleratorReadinessPolicy {
        kind,
        gpus,
        selected_gpus,
        max_memory_mib,
        max_util_percent,
        forbid_match,
        forbid_regex,
    })
}

fn build_accelerator_readiness_report(
    policy: &AcceleratorReadinessPolicy,
    status: AcceleratorStatusResponse,
    observed_stable_seconds: Option<u64>,
    timed_out: bool,
) -> AcceleratorReadinessReport {
    let mut blockers = Vec::new();
    let accelerator_label = accelerator_kind_label(&policy.kind);
    if !status.available {
        blockers.push(AcceleratorReadinessBlocker {
            kind: "accelerator_unavailable".to_string(),
            gpu_index: None,
            message: status
                .reason
                .clone()
                .unwrap_or_else(|| "accelerator unavailable".to_string()),
            memory_used_mib: None,
            max_memory_mib: None,
            utilization_percent: None,
            max_util_percent: None,
            pid: None,
            process_command: None,
            process_used_memory_mib: None,
            pattern: None,
        });
    }

    if let Some(selected) = &policy.selected_gpus {
        let present = status
            .devices
            .iter()
            .map(|device| device.index)
            .collect::<BTreeSet<_>>();
        for index in selected.difference(&present) {
            blockers.push(AcceleratorReadinessBlocker {
                kind: "gpu_missing".to_string(),
                gpu_index: Some(*index),
                message: format!("{accelerator_label} {index} was requested but not reported"),
                memory_used_mib: None,
                max_memory_mib: None,
                utilization_percent: None,
                max_util_percent: None,
                pid: None,
                process_command: None,
                process_used_memory_mib: None,
                pattern: None,
            });
        }
    }

    for device in &status.devices {
        match device.memory_used_mib {
            Some(used) if used <= policy.max_memory_mib => {}
            Some(used) => blockers.push(device_blocker(
                "memory_above_threshold",
                device.index,
                format!(
                    "{accelerator_label} {} memory {}MiB exceeds {}MiB",
                    device.index, used, policy.max_memory_mib
                ),
                Some(used),
                Some(policy.max_memory_mib),
                device.utilization_percent,
                Some(policy.max_util_percent),
            )),
            None => blockers.push(device_blocker(
                "memory_unknown",
                device.index,
                format!(
                    "{accelerator_label} {} memory usage is unknown",
                    device.index
                ),
                None,
                Some(policy.max_memory_mib),
                device.utilization_percent,
                Some(policy.max_util_percent),
            )),
        }

        match device.utilization_percent {
            Some(util) if util <= policy.max_util_percent => {}
            Some(util) => blockers.push(device_blocker(
                "utilization_above_threshold",
                device.index,
                format!(
                    "{accelerator_label} {} utilization {}% exceeds {}%",
                    device.index, util, policy.max_util_percent
                ),
                device.memory_used_mib,
                Some(policy.max_memory_mib),
                Some(util),
                Some(policy.max_util_percent),
            )),
            None => blockers.push(device_blocker(
                "utilization_unknown",
                device.index,
                format!(
                    "{accelerator_label} {} utilization is unknown",
                    device.index
                ),
                device.memory_used_mib,
                Some(policy.max_memory_mib),
                None,
                Some(policy.max_util_percent),
            )),
        }
    }

    if let Some(regex) = &policy.forbid_regex {
        for process in &status.processes {
            let command = process.command.as_deref().unwrap_or_default();
            if regex.is_match(command) {
                blockers.push(AcceleratorReadinessBlocker {
                    kind: "forbidden_process".to_string(),
                    gpu_index: process.gpu_index,
                    message: format!(
                        "{accelerator_label} process pid={} matches forbidden pattern",
                        process.pid
                    ),
                    memory_used_mib: None,
                    max_memory_mib: None,
                    utilization_percent: None,
                    max_util_percent: None,
                    pid: Some(process.pid),
                    process_command: process.command.clone(),
                    process_used_memory_mib: process.used_memory_mib,
                    pattern: policy.forbid_match.clone(),
                });
            }
        }
    }

    let passed = blockers.is_empty();
    AcceleratorReadinessReport {
        ok: passed && !timed_out,
        passed: passed && !timed_out,
        kind: policy.kind.clone(),
        available: status.available,
        provider: status.provider.clone(),
        gpus: policy.gpus.clone(),
        max_memory_mib: policy.max_memory_mib,
        max_util_percent: policy.max_util_percent,
        forbid_match: policy.forbid_match.clone(),
        stable_seconds: None,
        observed_stable_seconds,
        timed_out,
        blockers,
        agent_hint: readiness_agent_hint(&status),
        snapshot: status,
    }
}

fn device_blocker(
    kind: &str,
    gpu_index: u32,
    message: String,
    memory_used_mib: Option<u64>,
    max_memory_mib: Option<u64>,
    utilization_percent: Option<u32>,
    max_util_percent: Option<u32>,
) -> AcceleratorReadinessBlocker {
    AcceleratorReadinessBlocker {
        kind: kind.to_string(),
        gpu_index: Some(gpu_index),
        message,
        memory_used_mib,
        max_memory_mib,
        utilization_percent,
        max_util_percent,
        pid: None,
        process_command: None,
        process_used_memory_mib: None,
        pattern: None,
    }
}

fn readiness_agent_hint(status: &AcceleratorStatusResponse) -> String {
    if !status.available {
        return status.agent_hint.clone();
    }
    let accelerator_label = accelerator_kind_label(&status.kind);
    let (preflight_command, wait_command) = match status.kind {
        AcceleratorKind::Gpu => ("gpu-preflight", "gpu-wait-idle"),
        AcceleratorKind::Npu => (
            "accelerator-preflight --kind npu",
            "accelerator-wait-idle --kind npu",
        ),
    };
    format!(
        "Use {preflight_command} before starting {accelerator_label} workloads and {wait_command} after stopping them; avoid hand-written {accelerator_label} JSON polling unless custom policy is needed."
    )
}

fn print_accelerator_readiness_report(
    report: &AcceleratorReadinessReport,
    text: bool,
    command_name: &str,
) -> Result<()> {
    if text {
        print_accelerator_readiness_text(report, command_name);
    } else {
        println!("{}", serde_json::to_string_pretty(report)?);
    }
    if !report.passed {
        eprintln!("{command_name} failed:");
        for blocker in &report.blockers {
            eprintln!("  - {}", render_accelerator_blocker(blocker));
        }
        if report.timed_out {
            eprintln!("  - timed out before accelerator state stayed idle long enough");
        }
    }
    Ok(())
}

fn print_accelerator_readiness_text(report: &AcceleratorReadinessReport, command_name: &str) {
    if report.passed {
        println!(
            "{command_name} passed: memory <= {}MiB, utilization <= {}%",
            report.max_memory_mib, report.max_util_percent
        );
        if let Some(observed) = report.observed_stable_seconds {
            println!("Stable for {observed} second(s).");
        }
    } else {
        println!(
            "{command_name} failed: {} blocker(s).",
            report.blockers.len()
        );
        for blocker in &report.blockers {
            println!("  - {}", render_accelerator_blocker(blocker));
        }
        if report.timed_out {
            println!("Timed out before accelerator state stayed idle long enough.");
        }
    }
    println!("{}", report.agent_hint);
}

fn render_accelerator_blocker(blocker: &AcceleratorReadinessBlocker) -> String {
    let mut parts = Vec::new();
    parts.push(blocker.message.clone());
    if let Some(gpu) = blocker.gpu_index {
        parts.push(format!("device={gpu}"));
    }
    if let Some(used) = blocker.memory_used_mib {
        parts.push(format!("memory={used}MiB"));
    }
    if let Some(max) = blocker.max_memory_mib {
        parts.push(format!("max_memory={max}MiB"));
    }
    if let Some(util) = blocker.utilization_percent {
        parts.push(format!("util={util}%"));
    }
    if let Some(max) = blocker.max_util_percent {
        parts.push(format!("max_util={max}%"));
    }
    if let Some(pid) = blocker.pid {
        parts.push(format!("pid={pid}"));
    }
    if let Some(mem) = blocker.process_used_memory_mib {
        parts.push(format!("process_memory={mem}MiB"));
    }
    if let Some(pattern) = &blocker.pattern {
        parts.push(format!("pattern={pattern:?}"));
    }
    if let Some(command) = &blocker.process_command {
        parts.push(format!("cmd={command:?}"));
    }
    parts.join(" ")
}

fn report_exit_code(report: &AcceleratorReadinessReport) -> ExitCode {
    if report.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn parse_gpu_selection_arg(value: &str) -> Result<BTreeSet<u32>> {
    let mut selected = BTreeSet::new();
    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some((start, end)) = part.split_once('-') {
            let start = start
                .parse::<u32>()
                .with_context(|| format!("invalid GPU range start: {part}"))?;
            let end = end
                .parse::<u32>()
                .with_context(|| format!("invalid GPU range end: {part}"))?;
            if end < start {
                bail!("invalid GPU range: {part}");
            }
            for index in start..=end {
                selected.insert(index);
            }
        } else {
            selected.insert(
                part.parse::<u32>()
                    .with_context(|| format!("invalid GPU index: {part}"))?,
            );
        }
    }
    if selected.is_empty() {
        bail!("--gpus did not select any GPU");
    }
    Ok(selected)
}

fn accelerator_kind_label(kind: &AcceleratorKind) -> &'static str {
    match kind {
        AcceleratorKind::Gpu => "GPU",
        AcceleratorKind::Npu => "NPU",
    }
}

fn print_accelerator_device(kind: &AcceleratorKind, device: &AcceleratorDevice) {
    println!(
        "{} {}: {} memory={}MiB/{}MiB util={}%",
        accelerator_kind_label(kind),
        device.index,
        device.name,
        render_optional_u64(device.memory_used_mib),
        render_optional_u64(device.memory_total_mib),
        render_optional_u32(device.utilization_percent)
    );
}

fn print_accelerator_process(kind: &AcceleratorKind, process: &AcceleratorProcess) {
    println!(
        "  pid={} {}={} mem={}MiB cmd={}",
        process.pid,
        accelerator_kind_label(kind).to_ascii_lowercase(),
        process
            .gpu_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "?".to_string()),
        render_optional_u64(process.used_memory_mib),
        process.command.as_deref().unwrap_or("?")
    );
}

fn render_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn render_optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string())
}
