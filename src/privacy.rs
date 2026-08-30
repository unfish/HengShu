//! Explicit field masking, pseudonymization and output endpoint checks.

use crate::execution::{SensitiveLabel, Sensitivity};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Endpoint {
    LocalFile,
    Pdf,
    Stdout,
    Log,
    SmtpAttachment,
    SmtpBody,
    Webhook,
    EvidenceView,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointPolicy {
    pub allowed: BTreeSet<Endpoint>,
    pub sensitive_allowed: BTreeSet<SensitiveLabel>,
}

impl EndpointPolicy {
    pub fn all_endpoints() -> BTreeSet<Endpoint> {
        BTreeSet::from([
            Endpoint::LocalFile,
            Endpoint::Pdf,
            Endpoint::Stdout,
            Endpoint::Log,
            Endpoint::SmtpAttachment,
            Endpoint::SmtpBody,
            Endpoint::Webhook,
            Endpoint::EvidenceView,
        ])
    }

    pub fn permissive() -> Self {
        Self {
            allowed: Self::all_endpoints(),
            sensitive_allowed: BTreeSet::new(),
        }
    }

    pub fn allow(&self, endpoint: Endpoint, sensitivity: &Sensitivity) -> Result<(), String> {
        if !self.allowed.contains(&endpoint) {
            return Err(format!("端点未授权：{endpoint:?}"));
        }
        export_allowed(
            sensitivity,
            &self.sensitive_allowed.iter().cloned().collect::<Vec<_>>(),
            false,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaskStrategy {
    Full,
    KeepPrefix(usize),
    KeepSuffix(usize),
}

pub fn mask(value: &str, strategy: MaskStrategy) -> String {
    let count = value.chars().count();
    match strategy {
        MaskStrategy::Full => "*".repeat(count),
        MaskStrategy::KeepPrefix(prefix) => value
            .chars()
            .enumerate()
            .map(|(index, character)| if index < prefix { character } else { '*' })
            .collect(),
        MaskStrategy::KeepSuffix(suffix) => value
            .chars()
            .enumerate()
            .map(|(index, character)| {
                if index + suffix >= count {
                    character
                } else {
                    '*'
                }
            })
            .collect(),
    }
}

/// 掩码并登记专用脱敏来源节点；返回值与 `mask` 相同，附带节点 ID。
pub fn mask_with_provenance(
    value: &str,
    strategy: MaskStrategy,
    field: &str,
    metadata: &crate::execution::Metadata,
    graph: &mut crate::provenance::ProvenanceGraph,
    budget: crate::provenance::GraphBudget,
) -> Result<(String, String), String> {
    let masked = mask(value, strategy);
    let mut node_metadata = metadata.clone();
    node_metadata.sensitivity = node_metadata.sensitivity.clone().mask();
    let node = graph
        .add_masking(
            field,
            &format!("{strategy:?}"),
            metadata
                .provenance
                .iter()
                .map(|reference| reference.source_id.clone()),
            node_metadata,
            budget,
        )
        .map_err(|error| error.to_string())?;
    Ok((masked, node))
}

/// 假名化并登记专用脱敏来源节点；密钥只用于计算，不进入节点。
pub fn pseudonymize_with_provenance(
    value: &str,
    key: &[u8],
    field: &str,
    metadata: &crate::execution::Metadata,
    graph: &mut crate::provenance::ProvenanceGraph,
    budget: crate::provenance::GraphBudget,
) -> Result<(String, String), String> {
    let pseudonym = pseudonymize(value, key)?;
    let mut node_metadata = metadata.clone();
    node_metadata.sensitivity.pseudonymized = true;
    let node = graph
        .add_masking(
            field,
            "假名化",
            metadata
                .provenance
                .iter()
                .map(|reference| reference.source_id.clone()),
            node_metadata,
            budget,
        )
        .map_err(|error| error.to_string())?;
    Ok((pseudonym, node))
}

pub fn pseudonymize(value: &str, key: &[u8]) -> Result<String, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| "假名化密钥不能为空".to_string())?;
    mac.update(value.as_bytes());
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Detects the (practically unlikely but policy-relevant) case where distinct
/// inputs map to the same externally supplied pseudonym. A collision is never
/// silently accepted.
pub fn detect_pseudonym_collisions(
    values: impl IntoIterator<Item = String>,
    key: &[u8],
) -> Result<BTreeSet<String>, String> {
    let mut seen = BTreeMap::<String, String>::new();
    let mut collisions = BTreeSet::new();
    for value in values {
        let pseudonym = pseudonymize(&value, key)?;
        if seen
            .get(&pseudonym)
            .is_some_and(|previous| previous != &value)
        {
            collisions.insert(pseudonym);
        } else {
            seen.insert(pseudonym, value);
        }
    }
    Ok(collisions)
}

/// Place a number into a deterministic half-open bin. A non-positive width is
/// rejected so callers cannot accidentally create an unbounded/degenerate bin.
pub fn bin(value: f64, width: f64) -> Result<f64, String> {
    if !value.is_finite() || !width.is_finite() || width <= 0.0 {
        return Err("分箱值和箱宽必须是有限正数".into());
    }
    Ok((value / width).floor() * width)
}

/// Suppress groups smaller than `minimum_size`; the suppressed count is
/// returned separately so callers can report the loss without exposing rows.
pub fn enforce_minimum_group_size<T: Ord + Clone>(
    groups: BTreeMap<T, usize>,
    minimum_size: usize,
) -> Result<(BTreeMap<T, usize>, usize), String> {
    if minimum_size == 0 {
        return Err("最小组人数必须大于 0".into());
    }
    let mut kept = BTreeMap::new();
    let mut suppressed = 0usize;
    for (key, count) in groups {
        if count < minimum_size {
            suppressed = suppressed.saturating_add(count);
        } else {
            kept.insert(key, count);
        }
    }
    Ok((kept, suppressed))
}

thread_local! {
    // A policy belongs to one execution thread. A process-global policy lets
    // concurrent embedded runs overwrite each other's authority.
    static POLICY: RefCell<Option<EndpointPolicy>> = const { RefCell::new(None) };
}

/// 安装本次运行的端点策略。未安装表示沿用旧行为：不额外限制端点。
pub fn install_endpoint_policy(policy: EndpointPolicy) {
    POLICY.with(|current| *current.borrow_mut() = Some(policy));
}

pub fn clear_endpoint_policy() {
    POLICY.with(|current| *current.borrow_mut() = None);
}

/// 在实际外发前检查端点与敏感类别双重授权。没有敏感标签的数据不受限制。
pub fn guard_endpoint(endpoint: Endpoint, sensitivity: &Sensitivity) -> Result<(), String> {
    if sensitivity.labels.is_empty() || sensitivity.masked || sensitivity.pseudonymized {
        return Ok(());
    }
    POLICY.with(|current| match current.borrow().as_ref() {
        Some(policy) => policy.allow(endpoint, sensitivity),
        None => Ok(()),
    })
}

pub const REDACTED: &str = "<秘密已隐藏>";

static SECRETS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 登记一个秘密参数值，之后任何诊断、历史或崩溃文本都会用占位符替换它。
/// 过短的值不登记，否则会把普通文本大面积涂黑。
pub fn register_secret(value: &str) {
    if value.chars().count() < 4 {
        return;
    }
    if let Ok(mut secrets) = SECRETS.lock() {
        secrets.insert(value.to_string());
    }
}

pub fn scrub_secrets(text: &str) -> String {
    let Ok(secrets) = SECRETS.lock() else {
        return text.to_string();
    };
    let mut scrubbed = text.to_string();
    for secret in secrets.iter() {
        if scrubbed.contains(secret.as_str()) {
            scrubbed = scrubbed.replace(secret.as_str(), REDACTED);
        }
    }
    scrubbed
}

pub fn forget_secrets() {
    if let Ok(mut secrets) = SECRETS.lock() {
        secrets.clear();
    }
}

/// 崩溃信息经过同一套清理后才输出，避免 panic 负载带出秘密。
pub fn install_secret_scrubbing_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned());
        match payload {
            Some(payload) if payload != scrub_secrets(&payload) => {
                eprintln!(
                    "程序崩溃：{}{}",
                    scrub_secrets(&payload),
                    info.location()
                        .map(|location| format!("（{location}）"))
                        .unwrap_or_default()
                );
            }
            _ => previous(info),
        }
    }));
}

pub fn export_allowed(
    sensitivity: &Sensitivity,
    allowed: &[SensitiveLabel],
    masked_output: bool,
) -> Result<(), String> {
    if sensitivity.labels.is_empty()
        || sensitivity.masked
        || sensitivity.pseudonymized
        || masked_output
    {
        return Ok(());
    }
    if sensitivity
        .labels
        .iter()
        .all(|label| allowed.contains(label))
    {
        Ok(())
    } else {
        Err("输出包含未经授权的敏感字段".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn masking_and_pseudonymization_are_deterministic() {
        assert_eq!(
            mask("13800138000", MaskStrategy::KeepSuffix(4)),
            "*******8000"
        );
        assert_eq!(
            pseudonymize("张三", b"secret").unwrap(),
            pseudonymize("张三", b"secret").unwrap()
        );
        assert_ne!(
            pseudonymize("张三", b"secret").unwrap(),
            pseudonymize("张三", b"other").unwrap()
        );
    }

    #[test]
    fn endpoint_policy_rejects_unmasked_sensitive_values() {
        let sensitivity = Sensitivity {
            labels: BTreeSet::from([SensitiveLabel::Medical]),
            ..Default::default()
        };
        assert!(export_allowed(&sensitivity, &[], false).is_err());
        assert!(export_allowed(&sensitivity.clone().mask(), &[], false).is_ok());
        assert!(export_allowed(&sensitivity, &[SensitiveLabel::Medical], false).is_ok());
    }

    #[test]
    fn collision_binning_and_small_group_suppression_are_explicit() {
        assert!(
            detect_pseudonym_collisions(vec!["a".into(), "b".into()], b"key")
                .unwrap()
                .is_empty()
        );
        assert_eq!(bin(12.9, 10.0).unwrap(), 10.0);
        assert!(bin(1.0, 0.0).is_err());
        let groups = BTreeMap::from([("small", 2), ("large", 5)]);
        let (kept, suppressed) = enforce_minimum_group_size(groups, 3).unwrap();
        assert_eq!(kept, BTreeMap::from([("large", 5)]));
        assert_eq!(suppressed, 2);
    }

    #[test]
    fn registered_secrets_are_scrubbed_and_short_values_are_ignored() {
        forget_secrets();
        register_secret("超级机密口令值");
        register_secret("ab");
        assert_eq!(
            scrub_secrets("诊断：口令=超级机密口令值 结束"),
            format!("诊断：口令={REDACTED} 结束")
        );
        assert_eq!(scrub_secrets("ab 是普通文本"), "ab 是普通文本");
        forget_secrets();
        assert_eq!(scrub_secrets("超级机密口令值"), "超级机密口令值");
    }

    #[test]
    fn the_panic_hook_scrubs_registered_secrets_from_the_payload() {
        forget_secrets();
        register_secret("崩溃时的口令值");
        install_secret_scrubbing_panic_hook();
        let panicked = std::panic::catch_unwind(|| panic!("失败：崩溃时的口令值"));
        assert!(panicked.is_err());
        let _ = std::panic::take_hook();
        forget_secrets();
    }

    #[test]
    fn endpoint_policy_requires_both_endpoint_and_sensitive_authority() {
        let sensitivity = Sensitivity {
            labels: BTreeSet::from([SensitiveLabel::Medical]),
            ..Default::default()
        };
        let policy = EndpointPolicy {
            allowed: BTreeSet::from([Endpoint::Pdf]),
            sensitive_allowed: BTreeSet::new(),
        };
        assert!(policy.allow(Endpoint::Stdout, &sensitivity).is_err());
        assert!(policy.allow(Endpoint::Pdf, &sensitivity).is_err());
        let policy = EndpointPolicy {
            allowed: BTreeSet::from([Endpoint::Pdf]),
            sensitive_allowed: BTreeSet::from([SensitiveLabel::Medical]),
        };
        assert!(policy.allow(Endpoint::Pdf, &sensitivity).is_ok());
    }
}
