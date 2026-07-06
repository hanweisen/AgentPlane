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
        AcceleratorKind::Npu => handle_huawei_npu_status(state, payload).await,
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
    let selected_missing_reason =
        selected_missing_reason(AcceleratorKind::Gpu, &all_devices, payload.gpus.as_deref())?;
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

async fn handle_huawei_npu_status(
    state: &ServerState,
    payload: AcceleratorStatusRequest,
) -> Result<AcceleratorStatusResponse> {
    let npu_smi = state
        .npu_smi_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("npu-smi"));
    let npu_output = match run_npu_smi(&npu_smi, &["info"]).await {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(no_accelerator_response(
                AcceleratorKind::Npu,
                "npu-smi not found",
                "No NPU detected in this container. Do not retry NPU status checks unless the user says the environment changed.",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !npu_output.status.success() {
        let reason = String::from_utf8_lossy(&npu_output.stderr)
            .trim()
            .to_string();
        return Ok(no_accelerator_response(
            AcceleratorKind::Npu,
            if reason.is_empty() {
                "npu-smi failed"
            } else {
                &reason
            },
            "NPU status is unavailable because npu-smi failed. Do not retry NPU status checks unless the environment or driver state changes.",
        ));
    }

    let npu_output_text = String::from_utf8_lossy(&npu_output.stdout);
    let all_devices = parse_huawei_npu_devices(&npu_output_text);
    let provider_available = !all_devices.is_empty();
    let selected_missing_reason =
        selected_missing_reason(AcceleratorKind::Npu, &all_devices, payload.gpus.as_deref())?;
    let mut devices = all_devices;
    filter_devices(&mut devices, payload.gpus.as_deref())?;

    let mut processes = parse_huawei_npu_processes(&npu_output_text);
    enrich_processes_with_ps(&mut processes).await;
    filter_processes(
        &mut processes,
        payload.gpus.as_deref(),
        payload.process_match.as_deref(),
    )?;

    Ok(AcceleratorStatusResponse {
        ok: true,
        kind: AcceleratorKind::Npu,
        available: provider_available,
        provider: if provider_available {
            Some("huawei-ascend".to_string())
        } else {
            None
        },
        reason: if !provider_available {
            Some("npu-smi returned no NPU devices".to_string())
        } else {
            selected_missing_reason
        },
        devices,
        processes,
        agent_hint: if provider_available {
            "Huawei Ascend NPU accelerator detected. Use accelerator status data before NPU workloads and avoid repeated probes unless workload state changes.".to_string()
        } else {
            "No NPU detected in this container. Do not retry NPU status checks unless the user says the environment changed.".to_string()
        },
    })
}

fn selected_missing_reason(
    kind: AcceleratorKind,
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
    let label = accelerator_kind_label(&kind);
    Ok(Some(format!(
        "selected {label}(s) not reported: {}",
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

async fn run_npu_smi(npu_smi: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(npu_smi).args(args).output().await
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

fn parse_huawei_npu_devices(output: &str) -> Vec<AcceleratorDevice> {
    let mut devices = Vec::new();
    let mut pending: Option<(u32, String, Option<i32>, Option<u64>)> = None;
    for line in output.lines() {
        let Some(fields) = parse_table_fields(line) else {
            continue;
        };
        if fields
            .iter()
            .any(|field| field.to_ascii_lowercase().contains("process"))
        {
            break;
        }
        let Some(first_field) = fields.first() else {
            continue;
        };
        if first_field.starts_with("NPU") || first_field.starts_with("Chip") {
            continue;
        }
        let tokens = first_field.split_whitespace().collect::<Vec<_>>();
        let Some(first_index) = tokens.first().and_then(|value| parse_u32_field(value)) else {
            continue;
        };

        if let Some((npu_index, name, temperature_celsius, power_draw_milliwatts)) = pending.take()
        {
            let device_index = tokens
                .get(1)
                .and_then(|value| parse_u32_field(value))
                .unwrap_or(npu_index);
            let (utilization_percent, memory_used_mib, memory_total_mib) =
                parse_huawei_npu_usage_fields(&fields);
            devices.push(AcceleratorDevice {
                index: device_index,
                name,
                uuid: None,
                memory_used_mib,
                memory_total_mib,
                utilization_percent,
                pstate: None,
                power_draw_milliwatts,
                temperature_celsius,
            });
            continue;
        }

        if tokens.len() >= 2 {
            let name = tokens[1..].join(" ");
            let numbers = extract_decimal_numbers(
                &fields.iter().skip(2).cloned().collect::<Vec<_>>().join(" "),
            );
            let power_draw_milliwatts =
                numbers.first().map(|value| (value * 1000.0).round() as u64);
            let temperature_celsius = numbers.get(1).map(|value| value.round() as i32);
            pending = Some((
                first_index,
                name,
                temperature_celsius,
                power_draw_milliwatts,
            ));
        }
    }
    if let Some((index, name, temperature_celsius, power_draw_milliwatts)) = pending {
        devices.push(AcceleratorDevice {
            index,
            name,
            uuid: None,
            memory_used_mib: None,
            memory_total_mib: None,
            utilization_percent: None,
            pstate: None,
            power_draw_milliwatts,
            temperature_celsius,
        });
    }
    devices
}

fn parse_huawei_npu_usage_fields(fields: &[String]) -> (Option<u32>, Option<u64>, Option<u64>) {
    let Some(memory_field) = fields.iter().find(|field| field.contains('/')) else {
        return (None, None, None);
    };
    let numbers = extract_u64_numbers(memory_field);
    if numbers.len() < 2 {
        return (None, None, None);
    }
    let memory_used_mib = numbers.get(numbers.len() - 2).copied();
    let memory_total_mib = numbers.last().copied();
    let utilization_percent = numbers
        .get(numbers.len().saturating_sub(3))
        .and_then(|value| u32::try_from(*value).ok());
    (utilization_percent, memory_used_mib, memory_total_mib)
}

fn parse_huawei_npu_processes(output: &str) -> Vec<AcceleratorProcess> {
    output
        .lines()
        .filter_map(|line| {
            let fields = parse_table_fields(line)?;
            if fields
                .iter()
                .any(|field| field.to_ascii_lowercase().contains("process"))
            {
                return None;
            }
            let npu_index = fields
                .first()
                .and_then(|field| field.split_whitespace().next())
                .and_then(parse_u32_field);
            let (pid_field_index, command_field_index, memory_field_index) = if fields
                .get(1)
                .and_then(|field| parse_i32_field(field))
                .is_some()
            {
                (1, 2, 3)
            } else {
                (2, 3, 4)
            };
            let pid = fields
                .get(pid_field_index)
                .and_then(|field| parse_i32_field(field))?;
            let command = fields
                .get(command_field_index)
                .and_then(|field| parse_string_field(field));
            let used_memory_mib = fields
                .get(memory_field_index)
                .and_then(|field| extract_u64_numbers(field).first().copied());
            Some(AcceleratorProcess {
                pid,
                ppid: None,
                process_group_id: None,
                session_id: None,
                elapsed: None,
                stat: None,
                user: None,
                command,
                gpu_index: npu_index,
                gpu_uuid: None,
                used_memory_mib,
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

fn parse_table_fields(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || trimmed.contains("---") || trimmed.contains("===") {
        return None;
    }
    let fields = trimmed
        .trim_matches('|')
        .split('|')
        .map(|field| field.trim().to_string())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
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

fn extract_u64_numbers(value: &str) -> Vec<u64> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn extract_decimal_numbers(value: &str) -> Vec<f64> {
    value
        .split_whitespace()
        .filter_map(|part| {
            let normalized = part
                .trim_matches(|character: char| !character.is_ascii_digit() && character != '.');
            if normalized.is_empty() {
                None
            } else {
                normalized.parse().ok()
            }
        })
        .collect()
}

fn accelerator_kind_label(kind: &AcceleratorKind) -> &'static str {
    match kind {
        AcceleratorKind::Gpu => "GPU",
        AcceleratorKind::Npu => "NPU",
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
