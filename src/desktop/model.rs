//! Platform-neutral control mapping and desktop state transitions.

use crate::execution::{Artifact, ParameterSpec, ToolManifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControlKind {
    Text,
    Integer,
    Decimal,
    Date,
    DateTime,
    Boolean,
    Enum,
    File,
    Folder,
    Secret,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ControlSpec {
    pub name: String,
    pub label: String,
    pub kind: ControlKind,
    pub required: bool,
    pub secret: bool,
    pub description: String,
}

pub fn controls_for_tool(tool: &ToolManifest) -> Result<Vec<ControlSpec>, String> {
    tool.validate()?;
    tool.parameters
        .iter()
        .map(|(name, parameter)| {
            Ok(ControlSpec {
                name: name.clone(),
                label: name.clone(),
                kind: control_kind(parameter)?,
                required: parameter.required,
                secret: parameter.is_secret(),
                description: parameter.description.clone(),
            })
        })
        .collect()
}

fn control_kind(parameter: &ParameterSpec) -> Result<ControlKind, String> {
    let type_name = parameter.type_name.as_str();
    if type_name.starts_with("枚举:") || type_name.starts_with("enum:") {
        return Ok(ControlKind::Enum);
    }
    Ok(match type_name {
        "文本" | "text" | "字符串" => ControlKind::Text,
        "整数" | "integer" | "int" => ControlKind::Integer,
        "小数" | "数字" | "number" | "decimal" => ControlKind::Decimal,
        "日期" | "date" => ControlKind::Date,
        "时间" | "日期时间" | "datetime" => ControlKind::DateTime,
        "逻辑" | "布尔" | "bool" | "boolean" => ControlKind::Boolean,
        "路径" | "文件" | "path" | "file" => ControlKind::File,
        "文件夹" | "folder" => ControlKind::Folder,
        "秘密" | "secret" => ControlKind::Secret,
        other => return Err(format!("参数类型不能映射 GUI 控件：{other}")),
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DesktopState {
    Ready,
    Validating,
    Running {
        progress: Option<u8>,
    },
    Cancelling,
    Succeeded {
        artifacts: Vec<Artifact>,
    },
    Failed {
        message: String,
        diagnostics: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopEvent {
    Run,
    Cancel,
    Completed(Vec<Artifact>),
    Failed(String, Vec<String>),
    Reset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopModel {
    pub state: DesktopState,
    pub parameters: BTreeMap<String, serde_json::Value>,
    pub progress: Option<u8>,
}

impl Default for DesktopModel {
    fn default() -> Self {
        Self {
            state: DesktopState::Ready,
            parameters: BTreeMap::new(),
            progress: None,
        }
    }
}

impl DesktopModel {
    pub fn dispatch(&mut self, event: DesktopEvent) -> Result<(), String> {
        match (&self.state, event) {
            (DesktopState::Ready, DesktopEvent::Run) => {
                self.state = DesktopState::Validating;
                Ok(())
            }
            (DesktopState::Validating, DesktopEvent::Run) => {
                self.state = DesktopState::Running { progress: Some(0) };
                self.progress = Some(0);
                Ok(())
            }
            (DesktopState::Running { .. }, DesktopEvent::Cancel) => {
                self.state = DesktopState::Cancelling;
                Ok(())
            }
            (DesktopState::Cancelling, DesktopEvent::Completed(_)) => {
                self.state = DesktopState::Ready;
                self.progress = None;
                Ok(())
            }
            (DesktopState::Running { .. }, DesktopEvent::Completed(artifacts)) => {
                self.state = DesktopState::Succeeded { artifacts };
                self.progress = Some(100);
                Ok(())
            }
            (DesktopState::Running { .. }, DesktopEvent::Failed(message, diagnostics)) => {
                self.state = DesktopState::Failed {
                    message,
                    diagnostics,
                };
                Ok(())
            }
            (DesktopState::Succeeded { .. } | DesktopState::Failed { .. }, DesktopEvent::Reset) => {
                self.state = DesktopState::Ready;
                self.progress = None;
                Ok(())
            }
            _ => Err("GUI 状态不允许此操作".into()),
        }
    }
}
