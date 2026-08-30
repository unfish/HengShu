//! 值边界：范围枚举、结构化相等、可移植编码与统计量的数值性质。

use super::support::{configuration, run_vm, structured_errors, structured_value};
use crate::analysis::{array_method, to_array};
use crate::runtime::Interpreter;
use crate::security::PermissionSet;
use crate::value::{values_equal, PortableCodec, RangeValue, Value};
use proptest::prelude::*;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::rc::Rc;

proptest! {
    #![proptest_config(configuration())]

    #[test]
    fn finite_ranges_yield_exactly_the_stepped_elements(
        start in -100_i64..=100,
        end in -100_i64..=100,
        step in prop_oneof![-10_i64..=-1, 1_i64..=10],
        inclusive in any::<bool>(),
    ) {
        let range = Value::Range(Rc::new(RangeValue { start, end, inclusive, step }));
        let values = range.iter_values().unwrap();
        let span = if step > 0 { end - start } else { start - end };
        let magnitude = step.abs();
        let expected = if span < 0 || (span == 0 && !inclusive) {
            0
        } else if inclusive {
            span / magnitude + 1
        } else {
            (span + magnitude - 1) / magnitude
        };
        prop_assert_eq!(values.len() as i64, expected, "范围 {}..{} 步长 {}", start, end, step);
        for (offset, value) in values.iter().enumerate() {
            let expected = Value::Number(Decimal::from(start + offset as i64 * step));
            prop_assert!(values_equal(value, &expected), "第 {} 项应为 {:?}", offset + 1, expected);
        }
    }

    #[test]
    fn structured_values_are_equal_to_independent_copies(value in structured_value()) {
        // 逐字节重建一份结构相同但不共享 Rc 的值，否则 values_equal 会在指针相等上短路，
        // 断言就退化成恒真，任何结构化比较的回归都不会被发现。
        let codec = PortableCodec::new(crate::resource::DEFAULT_MAX_COLLECTION_ITEMS, 8);
        let copy = codec.decode(codec.encode(&value).unwrap()).unwrap();
        prop_assert!(values_equal(&value, &copy));
        prop_assert!(values_equal(&copy, &value));
    }

    #[test]
    fn statistical_mean_and_variance_obey_translation_properties(
        values in prop::collection::vec(-1_000_000.0_f64..1_000_000.0, 2..32),
        shift in -10_000.0_f64..10_000.0,
    ) {
        let value = Value::list(values.iter().copied().map(Value::Float).collect());
        let array = to_array(&value).unwrap();
        let mean = array_method(&array, "平均值", Vec::new(), BTreeMap::new())
            .unwrap()
            .as_f64()
            .unwrap();
        let variance = array_method(&array, "方差", Vec::new(), BTreeMap::new())
            .unwrap()
            .as_f64()
            .unwrap();

        let shifted = Value::list(
            values
                .iter()
                .map(|value| Value::Float(value + shift))
                .collect(),
        );
        let shifted = to_array(&shifted).unwrap();
        let shifted_mean = array_method(&shifted, "平均值", Vec::new(), BTreeMap::new())
            .unwrap()
            .as_f64()
            .unwrap();
        let shifted_variance = array_method(&shifted, "方差", Vec::new(), BTreeMap::new())
            .unwrap()
            .as_f64()
            .unwrap();

        let mean_tolerance = 1e-8 * (1.0 + mean.abs() + shift.abs());
        let variance_tolerance = 1e-8 * (1.0 + variance.abs());
        prop_assert!((shifted_mean - (mean + shift)).abs() <= mean_tolerance);
        prop_assert!(variance >= 0.0);
        prop_assert!((shifted_variance - variance).abs() <= variance_tolerance);
    }
}

#[test]
fn interpreter_and_vm_share_portable_value_boundaries() {
    let source = r#"
引入 并发
定义功能 原样（值）：
    返回 值
结束
定义功能 生成集合（）：
    返回 集合([3, 4])
结束
定义 通道值 为 并发 通道()
通道值 发送(集合([1, 2]))
输出(类型是(通道值 接收(), "集合"))
定义 参数任务 为 并发 启动(原样, [集合([1, 2])])
输出(类型是(参数任务 等待(), "集合"))
定义 返回任务 为 并发 启动(生成集合)
输出(类型是(返回任务 等待(), "集合"))
定义 映射结果 为 并发 并行映射([集合([1, 2])], 原样)
输出(类型是(映射结果[1], "集合"))
"#;
    let interpreter = Interpreter::execute_source(source, ".", false)
        .unwrap()
        .output;
    let vm = run_vm(source).unwrap();
    assert_eq!(interpreter, ["是", "是", "是", "是"]);
    assert_eq!(vm, interpreter);

    let cases = [
        (
            "引入 并发\n结构 客户：\n姓名\n结束\n定义 值 为 客户(姓名 为 \"甲\")\n定义 通道值 为 并发 通道()\n通道值 发送(值)",
            "结构实例暂不支持跨任务传递",
        ),
        (
            "引入 并发\n结构 客户：\n姓名\n结束\n定义功能 原样（值）：\n返回 值\n结束\n定义 值 为 客户(姓名 为 \"甲\")\n并发 启动(原样, [值])",
            "结构实例暂不支持跨任务传递",
        ),
        (
            "引入 并发\n结构 客户：\n姓名\n结束\n定义功能 原样（值）：\n返回 值\n结束\n定义 值 为 客户(姓名 为 \"甲\")\n并发 并行映射([值], 原样)",
            "结构实例暂不支持跨任务传递",
        ),
        (
            "引入 并发\n定义功能 工作（）：\n返回 工作\n结束\n定义 任务 为 并发 启动(工作)\n任务 等待()",
            "功能不能跨任务传递",
        ),
    ];
    for (source, core_message) in cases {
        let (interpreter, vm) =
            structured_errors(source, PermissionSet::for_source(std::path::Path::new(".")));
        for error in [&interpreter, &vm] {
            assert_eq!(error.category, "并发错误", "{}", error.message);
            assert_ne!(error.span.start.line, 0);
            assert!(error.message.contains(core_message), "{}", error.message);
        }
        assert_eq!(interpreter.span, vm.span);
    }
}
