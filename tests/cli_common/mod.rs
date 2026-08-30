//! 衡枢 CLI 集成测试共享工具。
#![allow(dead_code)]

pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_衡枢")
}

pub fn assert_single_location(rendered: &str, line: usize, fragment: &str) {
    assert!(rendered.contains(fragment), "{rendered}");
    assert!(rendered.contains(&format!("第 {line} 行")), "{rendered}");
    assert!(!rendered.contains("第 0 行"), "{rendered}");
    assert_eq!(rendered.matches(" 行发生").count(), 1, "{rendered}");
}
