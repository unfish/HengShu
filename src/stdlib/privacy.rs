use crate::privacy::{bin, mask, pseudonymize, MaskStrategy};
use crate::value::Value;
use std::collections::BTreeMap;

pub(super) fn privacy_call(
    name: &str,
    args: Vec<Value>,
    named: BTreeMap<String, Value>,
) -> Result<Value, String> {
    match name {
        "掩码" => {
            let text = args[0].to_string();
            let count = |key: &str| -> Result<Option<usize>, String> {
                named
                    .get(key)
                    .map(|value| {
                        crate::contracts::decimal_to_usize(value.as_decimal()?, "掩码保留位数")
                    })
                    .transpose()
            };
            let strategy = match (count("保留前")?, count("保留后")?) {
                (Some(_), Some(_)) => return Err("掩码只能保留前缀或后缀之一".into()),
                (Some(prefix), None) => MaskStrategy::KeepPrefix(prefix),
                (None, Some(suffix)) => MaskStrategy::KeepSuffix(suffix),
                (None, None) => MaskStrategy::Full,
            };
            Ok(Value::Text(mask(&text, strategy)))
        }
        "假名化" => {
            let key = match &args[1] {
                Value::Bytes(bytes) => bytes.borrow().clone(),
                value => value.to_string().into_bytes(),
            };
            Ok(Value::Text(pseudonymize(&args[0].to_string(), &key)?))
        }
        "分箱" => {
            let value = args[0].as_f64()?;
            let width = args[1].as_f64()?;
            Ok(Value::Float(bin(value, width)?))
        }
        _ => Err(format!("隐私模块不支持“{name}”")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_rejects_keeping_both_ends() {
        let error = privacy_call(
            "掩码",
            vec![Value::Text("13800001111".into())],
            BTreeMap::from([
                ("保留前".into(), Value::Number(3.into())),
                ("保留后".into(), Value::Number(4.into())),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("只能保留前缀或后缀之一"));
    }

    #[test]
    fn privacy_functions_agree_between_the_interpreter_and_the_vm() {
        let directory = tempfile::tempdir().unwrap();
        let source = "引入 隐私\n\
输出(隐私 掩码(\"110101199001011234\"))\n\
输出(隐私 掩码(\"13800001111\", 保留后 为 4))\n\
输出(隐私 掩码(\"13800001111\", 保留前 为 3))\n\
输出(隐私 假名化(\"张三\", \"盐值\"))\n\
输出(隐私 假名化(\"张三\", \"另一个盐\"))\n\
输出(隐私 分箱(37.4, 5))\n";
        let interpreted =
            crate::api::run_source(source, crate::api::RunOptions::new(directory.path()))
                .unwrap()
                .output;
        let compiled =
            crate::api::run_compiled(source, crate::api::RunOptions::new(directory.path()))
                .unwrap()
                .output;
        assert_eq!(interpreted, compiled);
        assert_eq!(interpreted[0], "******************");
        assert_eq!(interpreted[1], "*******1111");
        assert_eq!(interpreted[2], "138********");
        assert_ne!(interpreted[3], interpreted[4], "换密钥必须改变假名");
        assert_eq!(interpreted[5], "35");
    }
}
