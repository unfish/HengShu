use crate::value::Value;

pub(super) fn native_named_args(name: &str) -> &'static [&'static str] {
    if let Some(arguments) = crate::stdlib::named_args(name) {
        return arguments;
    }
    match name {
        "四舍五入" => &["位数"],
        "排序" => &["顺序"],
        "范围" => &["步长", "包含终点"],
        "错误" => &["类别"],
        "断言接近" => &["允许误差"],
        "断言会失败" => &["信息包含"],
        "统计分析.单样本T检验" => &["总体均值"],
        "统计分析.多重检验校正" => &["方法"],
        "报告.创建" => &["副标题", "主题"],
        "统计分析.正态分布概率" => &["均值", "标准差"],
        "统计分析.正态分布分位点" => &["概率"],
        "统计分析.均值置信区间" => &["置信度"],
        "统计分析.时间序列分解" => &["周期"],
        "统计分析.线性预测" => &["步数"],
        "统计分析.异常值检测" | "统计分析.排除异常值" | "统计分析.缩尾异常值" => {
            &["方法", "阈值"]
        }
        "统计分析.需求稳定性" => {
            &["最低周期数", "最低有销量率", "最大变异系数", "最低平均销量"]
        }
        "统计分析.需求预测" => &[
            "步数",
            "验证期",
            "移动窗口",
            "季节周期",
            "异常处理",
            "方法",
            "阈值",
        ],
        "统计分析.安全库存" => &[
            "预测需求",
            "服务水平",
            "提前期",
            "审查周期",
            "提前期标准差",
            "当前库存",
            "在途库存",
            "异常处理",
            "方法",
            "阈值",
        ],
        "统计分析.批量需求预测" => &[
            "商品列",
            "周期列",
            "销量列",
            "只保留稳定",
            "只保留可靠预测",
            "最大验证WAPE",
            "最低周期数",
            "最低有销量率",
            "最大变异系数",
            "最低平均销量",
            "步数",
            "验证期",
            "移动窗口",
            "季节周期",
            "异常处理",
            "方法",
            "阈值",
            "服务水平",
            "提前期",
            "审查周期",
            "提前期标准差",
            "当前库存",
            "在途库存",
        ],
        "统计分析.线性规划" => &["方向"],
        "统计分析.读取CSV分块" => &["块大小"],
        "统计分析.分块聚合" => &["统计量", "块大小", "工作者"],
        "统计分析.数据概览" => &["TopN"],
        "统计分析.期间比较" => &["周期列", "指标列", "分组列"],
        "统计分析.贡献分析" | "统计分析.Pareto分析" => &["分类列", "指标列"],
        "统计分析.队列分析" => &["实体列", "周期列"],
        "统计分析.漏斗分析" => &["实体列", "步骤列", "步骤"],
        "表格处理.读取表格" => &["最大行数", "工作表", "工作表序号", "表头行数", "模式"],
        "表格处理.写入表格" => &["单位schema"],
        "表格处理.分区写出" => &["分区列", "格式"],
        "并发.结束任务组" => &["取消"],
        "并发.并行映射" => &["工作者"],
        "财务.金额" => &["币种", "小数位", "舍入"],
        "财务.会计期间" => &["年度开始月"],
        "图表.折线图"
        | "图表.柱状图"
        | "图表.散点图"
        | "图表.直方图"
        | "图表.箱线图"
        | "图表.相关热力图"
        | "图表.堆叠柱状图"
        | "图表.瀑布图" => &[
            "标题",
            "横轴",
            "纵轴",
            "横轴名称",
            "纵轴名称",
            "系列",
            "单位",
            "小数位",
            "显示数值",
            "参考线",
            "区间下限",
            "区间上限",
            "注释",
        ],
        _ => &[],
    }
}

pub(super) fn method_named_args(value: &Value, name: &str) -> Option<&'static [&'static str]> {
    Some(match value {
        Value::Task(_) if name == "等待" => &["最长等待"],
        Value::Channel(_) if name == "接收" => &["最长等待"],
        Value::HttpResponse(_) if name == "转流" => &["块大小"],
        Value::Socket(_) if name == "接收" => &["最多字节"],
        Value::Table(_) => match name {
            "排序" => &["顺序"],
            "填补缺失" => &["值"],
            "分组统计" | "分组汇总" | "透视" => &["聚合"],
            "连接" => &["方式"],
            "排名" => &["结果列", "顺序"],
            "滚动统计" => &["聚合", "结果列"],
            "时间重采样" => &["频率"],
            _ => &[],
        },
        Value::NumericArray(_) => match name {
            "方差" | "标准差" | "标准偏差" => &["自由度"],
            "分位数" | "百分位数" => &["概率"],
            "自相关" => &["滞后"],
            "移动平均" => &["窗口"],
            "填补缺失" => &["值"],
            "直方统计" | "直方图" => &["分组数"],
            _ => &[],
        },
        Value::StructInstance(_) | Value::Module(_) => return None,
        _ => &[],
    })
}

pub(super) fn native_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "输出" => (0, None),
        "参数化输出" => (1, Some(1)),
        "输入" | "成功" | "失败" | "错误" => (0, Some(1)),
        "最大值" | "最小值" => (1, None),
        "四舍五入" | "断言" | "断言为空" | "断言不为空" => (1, Some(2)),
        "范围" => (2, Some(3)),
        "包含" | "类型是" => (2, Some(2)),
        "断言相等" | "断言不相等" | "断言包含" => (2, Some(3)),
        "断言接近" => (2, Some(2)),
        "断言会失败" => (1, Some(1)),
        "长度" | "绝对值" | "求和" | "平均值" | "排序" | "去重" | "转数字" | "转文本"
        | "转逻辑" | "字节" | "集合" | "路径" | "持续时间" | "流" => (1, Some(1)),
        "数学.平方根" => (1, Some(1)),
        "数学.随机数" => (0, Some(0)),
        "文本处理.大写" | "文本处理.小写" => (1, Some(1)),
        "文本处理.分割" => (2, Some(2)),
        "文本处理.替换" => (3, Some(3)),
        _ => return None,
    })
}

pub(super) fn method_arity(value: &Value, name: &str) -> Option<(usize, Option<usize>)> {
    Some(match value {
        Value::Money(_) => match name {
            "格式化" => (0, Some(0)),
            "按精度舍入" => (1, Some(1)),
            _ => return None,
        },
        Value::Task(_) => match name {
            "等待" => (0, Some(1)),
            "取消" => (0, Some(0)),
            _ => return None,
        },
        Value::Channel(_) => match name {
            "发送" => (1, Some(1)),
            "接收" | "尝试接收" | "关闭" => (0, Some(0)),
            _ => return None,
        },
        Value::HttpResponse(_) => match name {
            "保存到" => (1, Some(1)),
            "转流" => (0, Some(1)),
            "转JSON" | "JSON" => (0, Some(0)),
            _ => return None,
        },
        Value::Database(_) => match name {
            "执行" | "查询" | "流式查询" | "批量执行" => (1, Some(2)),
            "开始事务" | "提交" | "回滚" => (0, Some(0)),
            _ => return None,
        },
        Value::Socket(_) => match name {
            "发送" => (1, Some(1)),
            "发送到" => (2, Some(2)),
            "接收" => (0, Some(1)),
            "接受" => (0, Some(0)),
            _ => return None,
        },
        Value::Stream(_) => match name {
            "映射" | "筛选" | "取前" | "合并" => (1, Some(1)),
            "下一个" | "收集" => (0, Some(0)),
            _ => return None,
        },
        Value::List(_) => match name {
            "加入" | "删除" | "映射" | "筛选" => (1, Some(1)),
            "插入" => (2, Some(2)),
            "归并" => (1, Some(2)),
            "清空" => (0, Some(0)),
            _ => return None,
        },
        Value::Record(_) => match name {
            "删除" | "包含键" => (1, Some(1)),
            "转记录" | "键列表" | "值列表" => (0, Some(0)),
            _ => return None,
        },
        Value::Set(_) => match name {
            "加入" | "移除" | "包含" | "并集" | "交集" | "差集" => (1, Some(1)),
            _ => return None,
        },
        Value::Outcome(_) => match name {
            "取值" => (0, Some(0)),
            "失败时使用" => (1, Some(1)),
            _ => return None,
        },
        Value::Table(_) => match name {
            "设置列单位" => (2, Some(2)),
            "列单位" => (1, Some(1)),
            "设置列敏感" => (2, None),
            "列敏感" | "标记列脱敏" | "标记列假名" => (1, Some(1)),
            "筛选" | "排序" | "分组" | "选择列" | "去重" | "纵向合并" | "排名" | "校验" => {
                (1, Some(1))
            }
            "取前" => (1, Some(1)),
            "连接" | "滚动统计" | "时间重采样" => (2, Some(2)),
            "透视" => (3, Some(3)),
            "反透视" => (1, Some(2)),
            "填补缺失" | "分组统计" | "分组汇总" => (0, Some(1)),
            "转记录列表" | "删除缺失" | "相关矩阵" | "相关系数矩阵" | "描述" | "描述统计" => {
                (0, Some(0))
            }
            _ => return None,
        },
        Value::Grouped(_) => match name {
            "计数" => (0, Some(0)),
            "求和" | "平均" | "平均值" => (0, Some(1)),
            _ => return None,
        },
        Value::NumericArray(_) => match name {
            "相关系数" | "相关" | "协方差" | "矩阵乘" | "求解方程" => (1, Some(1)),
            "分位数" | "百分位数" | "直方统计" | "直方图" | "填补缺失" | "自相关" | "移动平均" => {
                (0, Some(1))
            }
            _ => (0, Some(0)),
        },
        Value::Regression(_) => (0, Some(0)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::runtime::interpreter::execute;

    #[test]
    fn invalid_builtin_arity_and_decimal_failures_are_language_errors() {
        assert!(execute("绝对值()", ".", false)
            .err()
            .unwrap()
            .contains("至少需要 1 个"));
        assert!(execute("输出(1 % 0)", ".", false)
            .err()
            .unwrap()
            .contains("不能对零取模"));
        assert!(
            execute("输出(79228162514264337593543950335 + 1)", ".", false)
                .err()
                .unwrap()
                .contains("溢出")
        );
        assert!(execute("绝对值(1, 拼错 为 2)", ".", false)
            .err()
            .unwrap()
            .contains("没有命名参数“拼错”"));
        assert!(execute(
            "引入 统计分析\n统计分析 正态分布概率(1, 拼错 为 2)",
            ".",
            false
        )
        .err()
        .unwrap()
        .contains("没有命名参数“拼错”"));
    }

    #[test]
    fn fractional_integer_inputs_are_rejected_instead_of_truncated() {
        for source in [
            "输出([1, 2][1.5])",
            "输出(范围(1.5, 3))",
            "引入 并发\n并发 休眠(1.5)",
            "定义 数据 为 [1]\n数据 插入(1.5, 2)",
        ] {
            let message = match execute(source, ".", false) {
                Ok(_) => panic!("源码应被拒绝：{source}"),
                Err(message) => message,
            };
            assert!(
                message.contains("必须是整数"),
                "源码：{source}\n错误：{message}"
            );
        }
    }
}
