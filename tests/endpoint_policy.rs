//! 端点策略测试：带敏感标签的数据在文件、PDF、日志、标准输出和凭证查看端点上
//! 必须同时通过端点授权和敏感类别授权。策略是进程级状态，所有断言放在一个测试里串行执行。

use hengshu::delivery::{document_from_data, FontResource, PdfBudget};
use hengshu::evidence::{EvidenceManifest, EVIDENCE_VERSION};
use hengshu::execution::{Metadata, ResourceSummary, SensitiveLabel, Sensitivity};
use hengshu::privacy::{clear_endpoint_policy, guard_endpoint, install_endpoint_policy};
use hengshu::privacy::{Endpoint, EndpointPolicy};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

/// 端点策略是进程级状态，测试线程必须串行进入。
static POLICY_LOCK: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    POLICY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn financial() -> Sensitivity {
    Sensitivity {
        labels: BTreeSet::from([SensitiveLabel::Financial]),
        masked: false,
        pseudonymized: false,
    }
}

fn policy(endpoints: &[Endpoint], labels: &[SensitiveLabel]) -> EndpointPolicy {
    EndpointPolicy {
        allowed: endpoints.iter().cloned().collect(),
        sensitive_allowed: labels.iter().cloned().collect(),
    }
}

fn manifest(
    parameters: BTreeMap<String, serde_json::Value>,
    metadata: Metadata,
) -> EvidenceManifest {
    EvidenceManifest {
        version: Default::default(),
        evidence_version: EVIDENCE_VERSION,
        run_id: "r".into(),
        program_digest: "d".into(),
        source_path: None,
        source_text: None,
        output: vec!["工资 12000".into()],
        success: true,
        permissions: None,
        bytecode_digest: None,
        parameters,
        inputs: Vec::new(),
        artifacts: Vec::new(),
        diagnostics: Vec::new(),
        external_dependencies: Vec::new(),
        resources: ResourceSummary::default(),
        metadata,
    }
}

#[test]
fn sensitive_data_needs_both_endpoint_and_label_authority() {
    let _serial = exclusive();
    let sensitivity = financial();

    // 没有安装策略时保持旧行为。
    clear_endpoint_policy();
    assert!(guard_endpoint(Endpoint::LocalFile, &sensitivity).is_ok());

    // 端点未列入允许集合：直接拒绝。
    install_endpoint_policy(policy(&[Endpoint::Stdout], &[SensitiveLabel::Financial]));
    let denied = guard_endpoint(Endpoint::LocalFile, &sensitivity).unwrap_err();
    assert!(denied.contains("端点未授权"), "{denied}");
    assert!(guard_endpoint(Endpoint::Stdout, &sensitivity).is_ok());

    // 端点允许但敏感类别未授权：仍然拒绝。
    install_endpoint_policy(policy(&[Endpoint::LocalFile, Endpoint::Pdf], &[]));
    assert!(guard_endpoint(Endpoint::LocalFile, &sensitivity).is_err());
    assert!(guard_endpoint(Endpoint::Pdf, &sensitivity).is_err());

    // 无标签数据以及已脱敏、已假名化的数据不受端点限制。
    assert!(guard_endpoint(Endpoint::Log, &Sensitivity::default()).is_ok());
    assert!(guard_endpoint(Endpoint::Log, &financial().mask()).is_ok());
    assert!(guard_endpoint(Endpoint::Webhook, &financial().pseudonymize()).is_ok());

    clear_endpoint_policy();
}

#[test]
fn pdf_artifact_refuses_unauthorized_endpoint() {
    let _serial = exclusive();
    let mut document = document_from_data(
        "工资报告",
        ["合计 12000".to_string()],
        Vec::new(),
        Vec::new(),
        FontResource {
            name: "Helvetica".into(),
            bytes: vec![1],
            embedded: true,
        },
    );
    document.metadata.sensitivity = financial();

    install_endpoint_policy(policy(&[Endpoint::LocalFile], &[SensitiveLabel::Financial]));
    let error = document
        .render_artifact("工资.pdf", &PdfBudget::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("端点未授权"), "{error}");

    install_endpoint_policy(policy(&[Endpoint::Pdf], &[SensitiveLabel::Financial]));
    let allowed = document.render_artifact("工资.pdf", &PdfBudget::default());
    assert!(allowed.is_ok(), "{:?}", allowed.err());
    clear_endpoint_policy();
}

#[test]
fn evidence_view_hides_parameters_and_output_without_authority() {
    let parameters = BTreeMap::from([(
        "身份证".to_string(),
        serde_json::Value::String("110101199001011234".into()),
    )]);
    let metadata = Metadata {
        sensitivity: financial(),
        ..Default::default()
    };
    let sensitive = manifest(parameters.clone(), metadata);

    let hidden = sensitive.view(&policy(
        &[Endpoint::LocalFile],
        &[SensitiveLabel::Financial],
    ));
    assert!(!hidden.parameters["身份证"]
        .as_str()
        .unwrap()
        .contains("110101"));
    assert_eq!(hidden.output.len(), 1);
    assert!(hidden.output[0].contains("敏感"));

    let visible = sensitive.view(&policy(
        &[Endpoint::EvidenceView],
        &[SensitiveLabel::Financial],
    ));
    assert_eq!(visible.parameters, parameters);
    assert_eq!(visible.output, sensitive.output);

    // 无敏感标签的凭证在任何策略下都完整可见。
    let plain = manifest(parameters.clone(), Metadata::default());
    assert_eq!(
        plain.view(&EndpointPolicy::default()).parameters,
        parameters
    );
}
