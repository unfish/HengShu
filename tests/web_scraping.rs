use hengshu::api::{check_source, run_source, RunOptions};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

const SOURCE: &str = r#"
引入 文件系统
引入 网页
引入 表格处理

定义 HTML 为 文件系统 读取文本("商品列表.html")
定义 字段 为 {
    "商品名称": "[class*='skuCard_details']",
    "规格": "[class*='skuCard_specTag']",
    "订货编码": "[class*='skuCard_skuNoBox'] span:first-of-type",
    "货期": "[class*='deliveryDate_deliveryDate'] .ml6",
    "原价": {"选择器": "[class*='skuCard_priceBox'] .tlt", "拒绝私用字符": 是},
    "优惠价": {"选择器": "[class*='skuCard_priceBox'] .ge5", "拒绝私用字符": 是}
}
定义 商品 为 网页 按字段采集(HTML, "[class*='skuCard_skuBox']", 字段)
遍历 商品，记作 行：
    如果 行 的 优惠价 == ""：
        行 的 价格状态 为 "页面价格受字体保护或需要登录"
    否则：
        行 的 价格状态 为 "已读取"
    结束
结束
定义 表 为 表格处理 把记录整理成表格(商品)
表格处理 写入表格(表, "商品.csv")
输出(长度(商品))
"#;

#[test]
fn fixture_extracts_products_and_writes_expected_csv() {
    check_source(SOURCE).unwrap();
    let directory = tempfile::tempdir().unwrap();
    fs::copy(
        "tests/data/工邦邦商品列表.html",
        directory.path().join("商品列表.html"),
    )
    .unwrap();
    let result = run_source(SOURCE, RunOptions::new(directory.path())).unwrap();
    assert_eq!(result.output, ["2"]);

    let csv = fs::read_to_string(directory.path().join("商品.csv")).unwrap();
    assert!(csv.contains("AC1368035"));
    assert!(csv.contains("19个自然日发货"));
    assert!(csv.contains("￥150.00"));
    assert!(csv.contains("￥128.00"));
    assert!(csv.contains("页面价格受字体保护或需要登录"));
    assert!(!csv.contains('\u{e232}'));
}

#[test]
fn lead_time_fixture_maps_the_public_response_fields() {
    let text = fs::read_to_string("tests/data/工邦邦货期响应.json").unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["code"], 200);
    assert_eq!(value["data"]["deliveryLT"], 19);
    assert_eq!(value["data"]["statusSignDesc"], "订货");
    assert_eq!(
        value["data"]["abLtDesc"],
        "19个自然日发货，预计09月06日送达"
    );
}

fn serve_pages(pages: Vec<(String, Vec<u8>, String)>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let count = pages.len();
    let pages = pages
        .into_iter()
        .map(|(path, body, content_type)| (path, (body, content_type)))
        .collect::<BTreeMap<_, _>>();
    let handle = thread::spawn(move || {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (status, body, content_type) = pages.get(path).map_or_else(
                || ("404 Not Found", b"not found".as_slice(), "text/plain"),
                |(body, content_type)| ("200 OK", body.as_slice(), content_type.as_str()),
            );
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        }
    });
    (format!("http://{address}"), handle)
}

fn network_options(directory: &std::path::Path) -> RunOptions {
    let mut options = RunOptions::new(directory);
    options.permissions.network_hosts.push("127.0.0.1".into());
    options
}

#[test]
fn gb18030_http_body_decodes_before_html_collection() {
    let fixture = fs::read_to_string("tests/data/GB18030图书页原文.html").unwrap();
    let (encoded, _, had_errors) = encoding_rs::GBK.encode(&fixture);
    assert!(!had_errors);
    let (base, server) = serve_pages(vec![(
        "/gb18030".into(),
        encoded.into_owned(),
        "text/html; charset=gb18030".into(),
    )]);
    let source = r#"
引入 网络
引入 网页
定义 响应 为 网络 请求("GET", "__BASE__/gb18030", 最长等待 为 持续时间("2秒"))
定义 图书 为 网页 按字段采集(响应 的 文本正文, ".book", {
    "书名": "h2",
    "出版社": ".publisher"
})
输出(图书[1] 的 书名, 图书[1] 的 出版社)
"#
    .replace("__BASE__", &base);
    let result = run_source(
        &source,
        network_options(tempfile::tempdir().unwrap().path()),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(result.output, ["财务分析方法 经济科学出版社"]);
}

#[test]
fn douban_three_page_fixture_normalizes_publication_fields_and_writes_csv() {
    let pages = (1..=3)
        .map(|page| {
            (
                format!("/douban?page={page}"),
                fs::read(format!("tests/data/豆瓣图书第{page}页.html")).unwrap(),
                "text/html; charset=utf-8".into(),
            )
        })
        .collect();
    let (base, server) = serve_pages(pages);
    let source = r#"
引入 网络
引入 网页
引入 表格处理
引入 文本处理
引入 正则
定义 全部图书 为 []
定义 页码 为 1
当 页码 <= 3 时重复：
    定义 响应 为 网络 请求("GET", "__BASE__/douban?page=" + 转文本(页码), 最长等待 为 持续时间("2秒"))
    定义 本页 为 网页 按字段采集(响应 的 文本正文, ".subject-item", {
        "书名": {"选择器": "h2 a", "属性": "title"},
        "出版信息": ".pub",
        "星级": ".rating_nums",
        "评价人数": ".pl",
        "缩略图地址": {"选择器": ".pic img", "属性": "src"}
    })
    遍历 本页，记作 图书：
        定义 信息 为 文本处理 分割(图书 的 出版信息, " / ")
        定义 数量 为 长度(信息)
        图书 的 价格 为 信息[数量]
        图书 的 出版日期 为 信息[数量 - 1]
        图书 的 出版社 为 信息[数量 - 2]
        图书 的 作者 为 ""
        定义 序号 为 1
        当 序号 <= 数量 - 3 时重复：
            如果 序号 > 1：
                图书 的 作者 为 图书 的 作者 + " / "
            结束
            图书 的 作者 为 图书 的 作者 + 信息[序号]
            序号 为 序号 + 1
        结束
        图书 的 评价人数 为 正则 查找("[0-9]+", 图书 的 评价人数)
        图书 的 分类标签 为 "小说"
        图书 的 装帧 为 ""
        图书 删除("出版信息")
        全部图书 加入(图书)
    结束
    页码 为 页码 + 1
结束
定义 表 为 表格处理 把记录整理成表格(全部图书)
表格处理 写入表格(表, "豆瓣.csv")
输出(长度(全部图书))
"#
    .replace("__BASE__", &base);
    let directory = tempfile::tempdir().unwrap();
    let result = run_source(&source, network_options(directory.path())).unwrap();
    server.join().unwrap();
    assert_eq!(result.output, ["3"]);
    let csv = fs::read_to_string(directory.path().join("豆瓣.csv")).unwrap();
    assert!(csv.contains("咸的玩笑"));
    assert!(csv.contains("8.4"));
    assert!(csv.contains("人民文学出版社"));
    assert!(csv.contains("[日] 东野圭吾 / 娄美莲"));
    assert!(csv.contains("170298"));
    assert!(csv.contains("湖南文艺出版社"));
}

#[test]
fn books_to_scrape_three_page_fixture_resolves_relative_urls_and_writes_csv() {
    let pages = (1..=3)
        .map(|page| {
            (
                format!("/catalogue/category/books_1/page-{page}.html"),
                fs::read(format!("tests/data/BooksToScrape第{page}页.html")).unwrap(),
                "text/html; charset=utf-8".into(),
            )
        })
        .collect();
    let (base, server) = serve_pages(pages);
    let source = r#"
引入 网络
引入 网页
引入 表格处理
引入 文本处理
定义 全部图书 为 []
定义 页码 为 1
当 页码 <= 3 时重复：
    定义 页面地址 为 "__BASE__/catalogue/category/books_1/page-" + 转文本(页码) + ".html"
    定义 响应 为 网络 请求("GET", 页面地址, 最长等待 为 持续时间("2秒"))
    定义 本页 为 网页 按字段采集(响应 的 文本正文, ".product_pod", {
        "书名": {"选择器": "h3 a", "属性": "title"},
        "价格": ".price_color",
        "星级": {"选择器": ".star-rating", "属性": "class"},
        "缩略图地址": {"选择器": ".image_container img", "属性": "src"},
        "详情地址": {"选择器": "h3 a", "属性": "href"}
    })
    遍历 本页，记作 图书：
        图书 的 星级 为 文本处理 替换(图书 的 星级, "star-rating ", "")
        图书 的 缩略图地址 为 网络 拼接URL(页面地址, 图书 的 缩略图地址)
        图书 的 详情地址 为 网络 拼接URL(页面地址, 图书 的 详情地址)
        图书 的 作者 为 ""
        图书 的 出版日期 为 ""
        图书 的 出版社 为 ""
        图书 的 装帧 为 ""
        图书 的 评价人数 为 ""
        图书 的 分类标签 为 "全部图书"
        全部图书 加入(图书)
    结束
    页码 为 页码 + 1
结束
定义 表 为 表格处理 把记录整理成表格(全部图书)
表格处理 写入表格(表, "英文图书.csv")
输出(长度(全部图书))
"#
    .replace("__BASE__", &base);
    let directory = tempfile::tempdir().unwrap();
    let result = run_source(&source, network_options(directory.path())).unwrap();
    server.join().unwrap();
    assert_eq!(result.output, ["3"]);
    let csv = fs::read_to_string(directory.path().join("英文图书.csv")).unwrap();
    assert!(csv.contains("A Light in the Attic"));
    assert!(csv.contains("Three"));
    assert!(csv.contains("/media/cache/light.jpg"));
    assert!(csv.contains("/catalogue/a-light-in-the-attic_1000/index.html"));
    assert!(csv.contains("Sapiens"));
    assert!(csv.contains("Poetry"));
}
