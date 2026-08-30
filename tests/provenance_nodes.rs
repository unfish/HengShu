//! 专用来源节点接入测试：对账、模板替换、报告 PDF、脱敏和单位转换各有独立节点，
//! 详细查看需要额外敏感权限。

use hengshu::execution::{Metadata, ProvenanceRef, SensitiveLabel};
use hengshu::privacy::{Endpoint, EndpointPolicy, MaskStrategy};
use hengshu::provenance::{
    GraphBudget, ProvenanceGraph, NODE_MASKING, NODE_RECONCILIATION, NODE_REPORT_PDF,
    NODE_TEMPLATE, NODE_UNIT_CONVERSION,
};
use std::collections::BTreeSet;

fn source_metadata(source: &str, location: &str) -> Metadata {
    let mut metadata = Metadata::default();
    metadata.provenance.push(ProvenanceRef {
        source_id: source.into(),
        location: Some(location.into()),
        step_id: Some(1),
    });
    metadata
}

#[test]
fn reconciliation_rows_get_dedicated_nodes_that_deduplicate() {
    use hengshu::reconciliation::{reconcile, Cell, ReconciliationRule, Record};
    let rule = ReconciliationRule {
        version: "1".into(),
        exact_keys: vec!["凭证号".into()],
        compare_fields: vec!["金额".into()],
        ..Default::default()
    };
    let record = |id: &str, voucher: &str, amount: &str| Record {
        id: id.into(),
        fields: [
            ("凭证号".to_string(), Cell::Text(voucher.into())),
            ("金额".to_string(), Cell::Text(amount.into())),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<String, Cell>>(),
        metadata: source_metadata("流水", "sheet:1!A2"),
        field_metadata: std::collections::BTreeMap::new(),
    };
    let rows = reconcile(
        &[record("l1", "V1", "100"), record("l2", "V2", "200")],
        &[record("r1", "V1", "100")],
        &rule,
    )
    .unwrap();
    let mut graph = ProvenanceGraph::default();
    let ids = hengshu::reconciliation::record_provenance(&mut graph, &rows, GraphBudget::default())
        .unwrap();
    assert_eq!(ids.len(), rows.len());
    assert!(graph
        .nodes
        .values()
        .all(|node| node.kind == NODE_RECONCILIATION));
    assert!(graph
        .nodes
        .values()
        .all(|node| node.parents == vec!["流水".to_string()]));
    let digest = graph.stable_digest();
    hengshu::reconciliation::record_provenance(&mut graph, &rows, GraphBudget::default()).unwrap();
    assert_eq!(graph.stable_digest(), digest, "重复登记不得改变来源图");
}

#[test]
fn report_pdf_masking_and_unit_conversion_have_separate_node_kinds() {
    use hengshu::delivery::{FontResource, PageSize, PdfBudget, ReportDocument, ReportElement};
    use hengshu::units::{Quantity, Unit};
    use rust_decimal::Decimal;

    let mut graph = ProvenanceGraph::default();
    let budget = GraphBudget::default();

    let document = ReportDocument {
        version: Default::default(),
        page_size: PageSize::A4,
        margin: 40,
        font: FontResource {
            name: "STSong-Light".into(),
            bytes: vec![1],
            embedded: true,
        },
        header: Some("经营报告".into()),
        footer: None,
        elements: vec![ReportElement::Paragraph {
            text: "华东毛利率 42%".into(),
            font_size: 12,
        }],
        metadata: source_metadata("经营表", "sheet:2!B3"),
    };
    let pdf_node = document
        .record_provenance(&mut graph, "报告.pdf", &PdfBudget::default(), budget)
        .unwrap();
    assert_eq!(graph.nodes[&pdf_node].kind, NODE_REPORT_PDF);
    assert!(graph.nodes[&pdf_node].label.contains("页数:"));

    let sensitive = {
        let mut metadata = source_metadata("患者表", "sheet:1!C4");
        metadata.sensitivity.labels.insert(SensitiveLabel::Medical);
        metadata
    };
    let (masked, mask_node) = hengshu::privacy::mask_with_provenance(
        "13800138000",
        MaskStrategy::KeepSuffix(4),
        "手机号",
        &sensitive,
        &mut graph,
        budget,
    )
    .unwrap();
    assert_eq!(masked, "*******8000");
    assert_eq!(graph.nodes[&mask_node].kind, NODE_MASKING);
    assert!(graph.nodes[&mask_node].metadata.sensitivity.masked);

    let (pseudonym, pseudonym_node) = hengshu::privacy::pseudonymize_with_provenance(
        "张三", b"key", "姓名", &sensitive, &mut graph, budget,
    )
    .unwrap();
    assert_eq!(pseudonym.len(), 64);
    assert!(
        graph.nodes[&pseudonym_node]
            .metadata
            .sensitivity
            .pseudonymized
    );
    assert!(!graph.nodes[&pseudonym_node].label.contains("张三"));

    let yuan = Unit::new(
        "元",
        "货币",
        Decimal::ONE,
        Decimal::ONE,
        Some("CNY".to_string()),
    )
    .unwrap();
    let wan = Unit::new(
        "万元",
        "货币",
        Decimal::new(10_000, 0),
        Decimal::ONE,
        Some("CNY".to_string()),
    )
    .unwrap();
    let (converted, unit_node) = hengshu::units::convert_with_provenance(
        &Quantity {
            value: Decimal::new(20_000, 0),
            unit: yuan,
        },
        &wan,
        None,
        "比例换算",
        &source_metadata("台账", "sheet:1!D5"),
        &mut graph,
        budget,
    )
    .unwrap();
    assert_eq!(converted.value, Decimal::new(2, 0));
    let unit_node = unit_node.unwrap();
    assert_eq!(graph.nodes[&unit_node].kind, NODE_UNIT_CONVERSION);
    assert!(graph.nodes[&unit_node]
        .label
        .contains("元->万元|依据:比例换算"));
}

#[test]
fn detailed_provenance_view_requires_extra_sensitive_permission() {
    let mut graph = ProvenanceGraph::default();
    let mut sensitive = source_metadata("患者表", "sheet:1!C4");
    sensitive.sensitivity.labels.insert(SensitiveLabel::Medical);
    graph
        .add_masking(
            "诊断",
            "原值",
            ["患者表".to_string()],
            sensitive,
            GraphBudget::default(),
        )
        .unwrap();

    let unauthorized = EndpointPolicy {
        allowed: BTreeSet::from([Endpoint::EvidenceView]),
        sensitive_allowed: BTreeSet::new(),
    };
    let redacted = graph.view(&unauthorized).unwrap();
    assert!(redacted
        .nodes
        .values()
        .all(|node| !node.label.contains("诊断")));
    assert!(redacted
        .nodes
        .values()
        .all(|node| node.label.contains("sheet:1!C4")));

    let authorized = EndpointPolicy {
        allowed: BTreeSet::from([Endpoint::EvidenceView]),
        sensitive_allowed: BTreeSet::from([SensitiveLabel::Medical]),
    };
    assert_eq!(graph.view(&authorized).unwrap(), graph);

    let without_endpoint = EndpointPolicy::default();
    let refused = graph.view(&without_endpoint).unwrap();
    assert!(refused
        .nodes
        .values()
        .all(|node| !node.label.contains("原值")));
}

#[test]
fn template_replacement_records_a_dedicated_node() {
    use hengshu::delivery_template::{
        replace_template_with_metadata, Replacement, ReplacementPlan, TemplateTarget,
    };
    use hengshu::template::TemplateBudget;
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("模板.docx");
    let output = directory.path().join("输出.docx");
    let file = std::fs::File::create(&input).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    std::io::Write::write_all(&mut zip, b"<Types/>").unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    std::io::Write::write_all(
        &mut zip,
        "<w:document><w:body><w:p><w:r><w:t>月份：{{month}}</w:t></w:r></w:p></w:body></w:document>"
            .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();

    let report = replace_template_with_metadata(
        &input,
        &output,
        &[ReplacementPlan {
            target: TemplateTarget::DocxPlaceholder {
                placeholder: "month".into(),
            },
            value: Replacement::Text("2026-08".into()),
        }],
        TemplateBudget::default(),
        source_metadata("台账", "sheet:1!A1"),
    )
    .unwrap();
    let mut graph = ProvenanceGraph::default();
    let node = hengshu::delivery_template::record_provenance(
        &mut graph,
        &input,
        &report,
        &[ReplacementPlan {
            target: TemplateTarget::DocxPlaceholder {
                placeholder: "month".into(),
            },
            value: Replacement::Text("2026-08".into()),
        }],
        GraphBudget::default(),
    )
    .unwrap();
    assert_eq!(graph.nodes[&node].kind, NODE_TEMPLATE);
    assert!(graph.nodes[&node].label.contains("输出.docx"));
    assert!(graph.nodes[&node].label.contains("month"));
    assert_eq!(graph.nodes[&node].parents, vec!["台账".to_string()]);
}

#[test]
fn the_provenance_graph_renders_a_readable_pdf_report_with_truncation() {
    use hengshu::delivery::{provenance_report, FontResource, PdfBudget, ReportElement};
    let mut graph = ProvenanceGraph::default();
    for index in 0..5 {
        graph
            .add_masking(
                &format!("字段{index}"),
                "Full",
                ["台账".to_string()],
                source_metadata("台账", "sheet:1!A1"),
                GraphBudget::default(),
            )
            .unwrap();
    }
    let font = FontResource {
        name: "STSong-Light".into(),
        bytes: vec![1],
        embedded: true,
    };
    let document = provenance_report(&graph, font.clone(), 3);
    let bytes = document.render_pdf(&PdfBudget::default()).unwrap();
    assert!(bytes.starts_with(b"%PDF"));
    assert!(document.elements.iter().any(|element| matches!(
        element,
        ReportElement::Paragraph { text, .. } if text.contains("节点总数：5")
    )));
    assert!(document.elements.iter().any(|element| matches!(
        element,
        ReportElement::Paragraph { text, .. } if text.contains("已截断")
    )));
    let table_rows = document
        .elements
        .iter()
        .find_map(|element| match element {
            ReportElement::Table { rows, .. } => Some(rows.len()),
            _ => None,
        })
        .unwrap();
    assert_eq!(table_rows, 3);

    let full = provenance_report(&graph, font, 100);
    assert!(!full.elements.iter().any(|element| matches!(
        element,
        ReportElement::Paragraph { text, .. } if text.contains("已截断")
    )));
}
