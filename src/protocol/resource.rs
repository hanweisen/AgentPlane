use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceClaim {
    pub kind: String,
    pub units: Vec<String>,
}

pub fn parse_resource_claim_specs(values: &[String]) -> Result<Vec<ResourceClaim>> {
    let mut claims = Vec::new();
    for value in values {
        let Some((kind, raw_units)) = value.split_once(':') else {
            bail!("expected KIND:UNIT[,UNIT...], got: {value}");
        };
        let units = raw_units
            .split(',')
            .map(str::trim)
            .filter(|unit| !unit.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        claims.push(ResourceClaim {
            kind: kind.to_string(),
            units,
        });
    }
    normalize_resource_claims(&claims)
}

pub fn normalize_resource_claims(claims: &[ResourceClaim]) -> Result<Vec<ResourceClaim>> {
    let mut merged = BTreeMap::<String, BTreeSet<String>>::new();
    for claim in claims {
        let kind = normalize_claim_kind(&claim.kind)?;
        if claim.units.is_empty() {
            bail!("resource claim {kind} must include at least one unit");
        }
        let units = merged.entry(kind.clone()).or_default();
        for unit in &claim.units {
            let normalized = normalize_claim_unit(unit)?;
            units.insert(normalized);
        }
    }
    Ok(merged
        .into_iter()
        .map(|(kind, units)| ResourceClaim {
            kind,
            units: units.into_iter().collect(),
        })
        .collect())
}

pub fn merge_resource_claims(
    explicit: &[ResourceClaim],
    inferred: &[ResourceClaim],
) -> Result<Vec<ResourceClaim>> {
    let explicit = normalize_resource_claims(explicit)?;
    let inferred = normalize_resource_claims(inferred)?;

    let mut merged = BTreeMap::<String, Vec<String>>::new();
    for claim in &explicit {
        merged.insert(claim.kind.clone(), claim.units.clone());
    }

    for claim in inferred {
        if let Some(existing_units) = merged.get(&claim.kind) {
            if *existing_units != claim.units {
                bail!(
                    "explicit resource claim {} conflicts with environment-inferred claim {}",
                    format_resource_claim(&ResourceClaim {
                        kind: claim.kind.clone(),
                        units: existing_units.clone(),
                    }),
                    format_resource_claim(&claim)
                );
            }
            continue;
        }
        merged.insert(claim.kind, claim.units);
    }

    Ok(merged
        .into_iter()
        .map(|(kind, units)| ResourceClaim { kind, units })
        .collect())
}

pub fn infer_gpu_resource_claims_from_process_env(
    env: Option<&std::collections::BTreeMap<String, Option<String>>>,
) -> Vec<ResourceClaim> {
    let Some(raw) = env
        .and_then(|env| env.get("CUDA_VISIBLE_DEVICES"))
        .and_then(|value| value.as_deref())
    else {
        return Vec::new();
    };
    infer_gpu_resource_claims(raw)
}

pub fn infer_gpu_resource_claims_from_sync_env(
    env: &std::collections::BTreeMap<String, String>,
) -> Vec<ResourceClaim> {
    let Some(raw) = env.get("CUDA_VISIBLE_DEVICES") else {
        return Vec::new();
    };
    infer_gpu_resource_claims(raw)
}

pub fn format_resource_claim(claim: &ResourceClaim) -> String {
    if claim.units.len() == 1 {
        return format!("{}:{}", claim.kind, claim.units[0]);
    }
    format!("{}:{}", claim.kind, claim.units.join(","))
}

fn infer_gpu_resource_claims(raw: &str) -> Vec<ResourceClaim> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "-1" {
        return Vec::new();
    }
    let units = trimmed
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if units.is_empty() {
        return Vec::new();
    }
    vec![ResourceClaim {
        kind: "gpu".to_string(),
        units,
    }]
}

fn normalize_claim_kind(kind: &str) -> Result<String> {
    let normalized = kind.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("resource claim kind must not be empty");
    }
    if normalized.contains(':')
        || normalized.contains(',')
        || normalized.contains(char::is_whitespace)
    {
        bail!("invalid resource claim kind: {kind}");
    }
    Ok(normalized)
}

fn normalize_claim_unit(unit: &str) -> Result<String> {
    let normalized = unit.trim().to_string();
    if normalized.is_empty() {
        bail!("resource claim unit must not be empty");
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{
        ResourceClaim, infer_gpu_resource_claims_from_sync_env, merge_resource_claims,
        normalize_resource_claims, parse_resource_claim_specs,
    };

    #[test]
    fn parse_resource_claim_specs_merges_repeated_kinds() {
        let parsed = parse_resource_claim_specs(&[
            "gpu:1,0".to_string(),
            "port:6006".to_string(),
            "gpu:0,2".to_string(),
        ])
        .expect("parse claims");

        assert_eq!(
            parsed,
            vec![
                ResourceClaim {
                    kind: "gpu".to_string(),
                    units: vec!["0".to_string(), "1".to_string(), "2".to_string()],
                },
                ResourceClaim {
                    kind: "port".to_string(),
                    units: vec!["6006".to_string()],
                },
            ]
        );
    }

    #[test]
    fn merge_resource_claims_rejects_gpu_conflict() {
        let explicit = vec![ResourceClaim {
            kind: "gpu".to_string(),
            units: vec!["1".to_string()],
        }];
        let inferred = infer_gpu_resource_claims_from_sync_env(
            &[("CUDA_VISIBLE_DEVICES".to_string(), "0".to_string())]
                .into_iter()
                .collect(),
        );

        let error = merge_resource_claims(&explicit, &inferred).expect_err("claim mismatch");
        assert!(error.to_string().contains(
            "explicit resource claim gpu:1 conflicts with environment-inferred claim gpu:0"
        ));
    }

    #[test]
    fn normalize_resource_claims_rejects_empty_units() {
        let error = normalize_resource_claims(&[ResourceClaim {
            kind: "gpu".to_string(),
            units: Vec::new(),
        }])
        .expect_err("missing units");
        assert!(
            error
                .to_string()
                .contains("resource claim gpu must include at least one unit")
        );
    }
}
