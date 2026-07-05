use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::ServerState;
use super::util::parse_i32_field;
use crate::protocol::{
    AcceleratorDevice, AcceleratorKind, AcceleratorProcess, AcceleratorStatusRequest,
    AcceleratorStatusResponse,
};

pub async fn handle_accelerator_status(
    state: &ServerState,
    payload: AcceleratorStatusRequest,
) -> Result<AcceleratorStatusResponse> {
    match payload.kind {
        AcceleratorKind::Gpu => handle_nvidia_gpu_status(state, payload).await,
        AcceleratorKind::Npu => Ok(AcceleratorStatusResponse {
            ok: true,
            kind: AcceleratorKind::Npu,
            available: false,
            provider: None,
            reason: Some("NPU accelerator provider is not implemented yet".to_string()),
            devices: Vec::new(),
            processes: Vec::new(),
            agent_hint: "No NPU provider is implemented in this AgentPlane build. Do not retry NPU status checks unless the binary is upgraded.".to_string(),
        }),
    }
}

async fn handle_nvidia_gpu_status(
    state: &ServerState,
    payload: AcceleratorStatusRequest,
) -> Result<AcceleratorStatusResponse> {
    let nvidia_smi = state
        .nvidia_smi_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("nvidia-smi"));
    let gpu_query = run_nvidia_smi(
        &nvidia_smi,
        &[
            "--query-gpu=index,name,uuid,memory.used,memory.total,utilization.gpu,pstate,power.draw,temperature.gpu",
            "--format=csv,noheader,nounits",
        ],
    )
    .await;
    let gpu_output = match gpu_query {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(no_accelerator_response(
                AcceleratorKind::Gpu,
                "nvidia-smi not found",
                "No GPU detected in this container. Do not retry GPU status checks unless the user says the environment changed.",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !gpu_output.status.success() {
        let reason = String::from_utf8_lossy(&gpu_output.stderr)
            .trim()
            .to_string();
        return Ok(no_accelerator_response(
            AcceleratorKind::Gpu,
            if reason.is_empty() {
                "nvidia-smi failed"
            } else {
                &reason
            },
            "GPU status is unavailable because nvidia-smi failed. Do not retry GPU status checks unless the environment or driver state changes.",
        ));
    }

    let all_devices = parse_nvidia_gpu_devices(&String::from_utf8_lossy(&gpu_output.stdout));
    let provider_available = !all_devices.is_empty();
    let selected_missing_reason = selected_missing_reason(&all_devices, payload.gpus.as_deref())?;
    let mut devices = all_devices;
    filter_devices(&mut devices, payload.gpus.as_deref())?;
    let uuid_to_index = devices
        .iter()
        .filter_map(|device| {
            device
                .uuid
                .as_ref()
                .map(|uuid| (uuid.clone(), device.index))
        })
        .collect::<BTreeMap<_, _>>();

    let process_output = run_nvidia_smi(
        &nvidia_smi,
        &[
            "--query-compute-apps=gpu_uuid,pid,used_memory",
            "--format=csv,noheader,nounits",
        ],
    )
    .await;
    let mut processes = match process_output {
        Ok(output) if output.status.success() => {
            parse_nvidia_compute_processes(&String::from_utf8_lossy(&output.stdout), &uuid_to_index)
        }
        _ => Vec::new(),
    };
    enrich_processes_with_ps(&mut processes).await;
    filter_processes(
        &mut processes,
        payload.gpus.as_deref(),
        payload.process_match.as_deref(),
    )?;

    Ok(AcceleratorStatusResponse {
        ok: true,
        kind: AcceleratorKind::Gpu,
        available: provider_available,
        provider: if provider_available {
            Some("nvidia".to_string())
        } else {
            None
        },
        reason: if !provider_available {
            Some("nvidia-smi returned no GPU devices".to_string())
        } else {
            selected_missing_reason
        },
        devices,
        processes,
        agent_hint: if provider_available {
            "GPU accelerator detected. Use accelerator status data before GPU workloads and avoid repeated probes unless workload state changes.".to_string()
        } else {
            "No GPU detected in this container. Do not retry GPU status checks unless the user says the environment changed.".to_string()
        },
    })
}

fn selected_missing_reason(
    devices: &[AcceleratorDevice],
    gpus: Option<&str>,
) -> Result<Option<String>> {
    let Some(gpus) = gpus else {
        return Ok(None);
    };
    let selected = parse_gpu_selection(gpus)?;
    let present = devices
        .iter()
        .map(|device| device.index)
        .collect::<BTreeSet<_>>();
    let missing = selected.difference(&present).collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "selected GPU(s) not reported: {}",
        missing
            .iter()
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )))
}

fn no_accelerator_response(
    kind: AcceleratorKind,
    reason: &str,
    agent_hint: &str,
) -> AcceleratorStatusResponse {
    AcceleratorStatusResponse {
        ok: true,
        kind,
        available: false,
        provider: None,
        reason: Some(reason.to_string()),
        devices: Vec::new(),
        processes: Vec::new(),
        agent_hint: agent_hint.to_string(),
    }
}

async fn run_nvidia_smi(nvidia_smi: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(nvidia_smi).args(args).output().await
}

fn parse_nvidia_gpu_devices(output: &str) -> Vec<AcceleratorDevice> {
    output
        .lines()
        .filter_map(|line| {
            let fields = parse_csv_line(line);
            if fields.len() < 9 {
                return None;
            }
            Some(AcceleratorDevice {
                index: parse_u32_field(&fields[0])?,
                name: fields[1].clone(),
                uuid: parse_string_field(&fields[2]),
                memory_used_mib: parse_u64_field(&fields[3]),
                memory_total_mib: parse_u64_field(&fields[4]),
                utilization_percent: parse_u32_field(&fields[5]),
                pstate: parse_string_field(&fields[6]),
                power_draw_milliwatts: parse_power_milliwatts(&fields[7]),
                temperature_celsius: parse_i32_field(&fields[8]),
            })
        })
        .collect()
}

fn parse_nvidia_compute_processes(
    output: &str,
    uuid_to_index: &BTreeMap<String, u32>,
) -> Vec<AcceleratorProcess> {
    output
        .lines()
        .filter_map(|line| {
            let fields = parse_csv_line(line);
            if fields.len() < 3 {
                return None;
            }
            let gpu_uuid = parse_string_field(&fields[0])?;
            let pid = parse_i32_field(&fields[1])?;
            Some(AcceleratorProcess {
                pid,
                ppid: None,
                process_group_id: None,
                session_id: None,
                elapsed: None,
                stat: None,
                user: None,
                command: None,
                gpu_index: uuid_to_index.get(&gpu_uuid).copied(),
                gpu_uuid: Some(gpu_uuid),
                used_memory_mib: parse_u64_field(&fields[2]),
            })
        })
        .collect()
}

async fn enrich_processes_with_ps(processes: &mut [AcceleratorProcess]) {
    if processes.is_empty() {
        return;
    }
    let pid_list = processes
        .iter()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let output = process_detail_output(&pid_list).await;
    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let rows = String::from_utf8_lossy(&output.stdout);
    let mut details = BTreeMap::new();
    for row in rows.lines() {
        let fields = row.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 7 {
            continue;
        }
        let Some(pid) = parse_i32_field(fields[0]) else {
            continue;
        };
        let command = row.split_whitespace().skip(7).collect::<Vec<_>>().join(" ");
        details.insert(
            pid,
            (
                parse_i32_field(fields[1]),
                parse_i32_field(fields[2]),
                parse_i32_field(fields[3]),
                Some(fields[4].to_string()),
                Some(fields[5].to_string()),
                Some(fields[6].to_string()),
                if command.is_empty() {
                    None
                } else {
                    Some(command)
                },
            ),
        );
    }
    for process in processes {
        if let Some((ppid, pgid, sid, elapsed, stat, user, command)) = details.remove(&process.pid)
        {
            process.ppid = ppid;
            process.process_group_id = pgid;
            process.session_id = sid;
            process.elapsed = elapsed;
            process.stat = stat;
            process.user = user;
            process.command = command;
        }
    }
}

async fn process_detail_output(pid_list: &str) -> std::io::Result<std::process::Output> {
    let output = Command::new("ps")
        .args([
            "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "sid=", "-o", "etime=", "-o",
            "stat=", "-o", "user=", "-o", "command=", "-p", pid_list,
        ])
        .output()
        .await?;
    if output.status.success() {
        return Ok(output);
    }
    Command::new("ps")
        .args([
            "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "sess=", "-o", "etime=", "-o",
            "stat=", "-o", "user=", "-o", "command=", "-p", pid_list,
        ])
        .output()
        .await
}

fn filter_devices(devices: &mut Vec<AcceleratorDevice>, gpus: Option<&str>) -> Result<()> {
    let Some(gpus) = gpus else {
        return Ok(());
    };
    let selected = parse_gpu_selection(gpus)?;
    devices.retain(|device| selected.contains(&device.index));
    Ok(())
}

fn filter_processes(
    processes: &mut Vec<AcceleratorProcess>,
    gpus: Option<&str>,
    process_match: Option<&str>,
) -> Result<()> {
    if let Some(gpus) = gpus {
        let selected = parse_gpu_selection(gpus)?;
        processes.retain(|process| {
            process
                .gpu_index
                .is_some_and(|index| selected.contains(&index))
        });
    }
    if let Some(process_match) = process_match {
        processes.retain(|process| {
            process
                .command
                .as_deref()
                .unwrap_or_default()
                .contains(process_match)
        });
    }
    Ok(())
}

fn parse_gpu_selection(value: &str) -> Result<BTreeSet<u32>> {
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
    Ok(selected)
}

fn parse_csv_line(line: &str) -> Vec<String> {
    line.split(',')
        .map(|field| field.trim().to_string())
        .collect()
}

fn parse_string_field(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("N/A")
        || value.eq_ignore_ascii_case("[Not Supported]")
    {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_u64_field(value: &str) -> Option<u64> {
    parse_string_field(value)?.parse().ok()
}

fn parse_u32_field(value: &str) -> Option<u32> {
    parse_string_field(value)?.parse().ok()
}

fn parse_power_milliwatts(value: &str) -> Option<u64> {
    let value = parse_string_field(value)?;
    let watts = value.parse::<f64>().ok()?;
    Some((watts * 1000.0).round() as u64)
}
