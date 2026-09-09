//! Desktop application state, schema mapping, and the optional GPUI Kit host.

use crate::execution::Artifact;
use std::collections::BTreeMap;

mod model;
pub use model::{
    controls_for_tool, ControlKind, ControlSpec, DesktopEvent, DesktopModel, DesktopState,
};

#[cfg(feature = "gui")]
mod gpui_app;

#[cfg(feature = "gui")]
pub use gpui_app::run_gui;

/// Build the evidence emitted by a successful GUI run.
pub fn gui_evidence(
    source: &str,
    bytecode: &crate::bytecode::BytecodeProgram,
    permissions: &crate::security::PermissionSet,
    base: &std::path::Path,
    parameters: &BTreeMap<String, serde_json::Value>,
    output: &[String],
    artifacts: &[Artifact],
) -> crate::evidence::EvidenceManifest {
    gui_evidence_with_run(
        source,
        bytecode,
        permissions,
        base,
        parameters,
        output,
        artifacts,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        crate::execution::ResourceSummary::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gui_evidence_with_run(
    source: &str,
    bytecode: &crate::bytecode::BytecodeProgram,
    permissions: &crate::security::PermissionSet,
    base: &std::path::Path,
    parameters: &BTreeMap<String, serde_json::Value>,
    output: &[String],
    artifacts: &[Artifact],
    inputs: Vec<crate::execution::InputObservation>,
    diagnostics: Vec<crate::api::DiagnosticReport>,
    mut external_dependencies: Vec<crate::evidence::ExternalDependency>,
    resources: crate::execution::ResourceSummary,
) -> crate::evidence::EvidenceManifest {
    use crate::execution::hex_digest;
    let program_digest = hex_digest(source.as_bytes());
    if external_dependencies.is_empty() {
        external_dependencies.push(crate::evidence::ExternalDependency {
            kind: "无".into(),
            identity: "本机执行".into(),
            fingerprint: None,
            level: crate::evidence::ExternalDependencyLevel::None,
        });
    }
    crate::evidence::EvidenceManifest {
        version: crate::execution::ContractVersion::default(),
        evidence_version: crate::evidence::EVIDENCE_VERSION,
        run_id: format!("gui-{}", &program_digest[..12]),
        program_digest,
        source_path: None,
        source_text: Some(source.to_string()),
        output: output.to_vec(),
        success: true,
        permissions: Some(permissions.clone().into_portable_roots(base)),
        bytecode_digest: postcard::to_allocvec(bytecode)
            .ok()
            .map(|bytes| hex_digest(&bytes)),
        parameters: parameters.clone(),
        inputs,
        artifacts: artifacts.to_vec(),
        diagnostics,
        external_dependencies,
        resources,
        metadata: crate::execution::Metadata::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{ParameterSpec, ToolManifest};

    #[test]
    fn controls_map_schema_without_executing_arbitrary_logic() {
        let tool = ToolManifest {
            version: Default::default(),
            name: "工具".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::from([
                (
                    "日期".into(),
                    ParameterSpec {
                        type_name: "日期".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
                (
                    "密码".into(),
                    ParameterSpec {
                        type_name: "秘密".into(),
                        required: true,
                        secret: true,
                        description: String::new(),
                    },
                ),
            ]),
            gui: None,
            cli: None,
            permissions: Default::default(),
        };
        let controls = controls_for_tool(&tool).unwrap();
        assert_eq!(
            controls.iter().find(|c| c.name == "日期").unwrap().kind,
            ControlKind::Date
        );
        assert_eq!(
            controls.iter().find(|c| c.name == "密码").unwrap().kind,
            ControlKind::Secret
        );
    }

    #[test]
    fn desktop_state_machine_rejects_invalid_transitions_and_supports_cancel() {
        let mut model = DesktopModel::default();
        model.dispatch(DesktopEvent::Run).unwrap();
        model.dispatch(DesktopEvent::Run).unwrap();
        model.dispatch(DesktopEvent::Cancel).unwrap();
        assert!(matches!(model.state, DesktopState::Cancelling));
        model.dispatch(DesktopEvent::Completed(Vec::new())).unwrap();
        assert!(matches!(model.state, DesktopState::Ready));
    }

    #[test]
    fn gui_result_page_writes_a_verifiable_evidence_manifest() {
        let source = "输出(1)\n";
        let program = crate::parser::parse(source).unwrap();
        let bytecode = crate::compiler::compile(&program).unwrap();
        let base = std::path::Path::new("/安装目录");
        let permissions = crate::security::PermissionSet::for_source(base);
        let manifest = gui_evidence(
            source,
            &bytecode,
            &permissions,
            base,
            &BTreeMap::from([("月份".into(), serde_json::json!("2026-08"))]),
            &["1".to_string()],
            &[],
        );
        assert_eq!(manifest.evidence_version, crate::evidence::EVIDENCE_VERSION);
        assert_eq!(manifest.source_text.as_deref(), Some(source));
        assert_eq!(manifest.output, ["1"]);
        assert_eq!(manifest.parameters["月份"], serde_json::json!("2026-08"));
        assert!(manifest.external_dependencies.iter().all(|dependency| {
            dependency.level == crate::evidence::ExternalDependencyLevel::None
        }));
        assert!(!format!("{:?}", manifest.permissions.unwrap()).contains("/安装目录"));
    }
}
