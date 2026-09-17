//! export 模块的单元测试：标点规范化、TXT 渲染与 EPUB 回填/生成。

use super::epub::rewrite_xhtml;
use super::punctuation::normalize_chinese_punctuation;
use super::{render_epub, render_txt};
use crate::model::{Chapter, ItemStatus, ProjectState, ProjectStatus, Segment, SegmentKind};
use crate::state::ExportSnapshot;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn snapshot() -> ExportSnapshot {
    ExportSnapshot {
        project: ProjectState {
            id: "a".repeat(64),
            title: "Book".to_string(),
            source_file: "book.txt".to_string(),
            source_path: "/book.txt".to_string(),
            source_hash: "a".repeat(64),
            source_language: "en".to_string(),
            target_language: "zh-CN".to_string(),
            status: ProjectStatus::Translated,
            chapters_total: 1,
            chapters_completed: 1,
            created_at: String::new(),
            updated_at: String::new(),
            max_segment_chars: 1_200,
        },
        chapters: vec![Chapter {
            id: "chapter-1".to_string(),
            title: "Chapter 1".to_string(),
            target_title: Some("第一章".to_string()),
            status: ItemStatus::Translated,
            meta: serde_json::json!({}),
            segments: vec![
                Segment {
                    id: "heading".to_string(),
                    ordinal: 0,
                    source: "Chapter 1".to_string(),
                    target: Some("第一章".to_string()),
                    target_before_polish: None,
                    polish_status: None,
                    kind: SegmentKind::Heading,
                    status: ItemStatus::Translated,
                    source_hash: String::new(),
                    meta: serde_json::json!({}),
                },
                Segment {
                    id: "body".to_string(),
                    ordinal: 1,
                    source: "Hello, world!".to_string(),
                    target: Some("你好, 世界!".to_string()),
                    target_before_polish: None,
                    polish_status: None,
                    kind: SegmentKind::Paragraph,
                    status: ItemStatus::Translated,
                    source_hash: String::new(),
                    meta: serde_json::json!({}),
                },
            ],
        }],
        source_bytes: Vec::new(),
    }
}

#[test]
fn punctuation_normalization_is_conservative() {
    assert_eq!(
        normalize_chinese_punctuation("你好 , world: https://a.b/x? ... -- `a:b?` ok"),
        "你好， world： https://a.b/x?……—— `a:b?` ok"
    );
}

#[test]
fn txt_export_orders_content_and_avoids_duplicate_heading() {
    let rendered = render_txt(&snapshot()).expect("TXT should render");
    assert_eq!(rendered, "第一章\n\n你好， 世界！\n");
}

#[test]
fn txt_export_rejects_incomplete_translation() {
    let mut snapshot = snapshot();
    snapshot.chapters[0].segments[1].target = None;
    let error = render_txt(&snapshot).expect_err("incomplete export should fail");
    assert!(error.contains("empty translation"));
}

#[test]
fn epub_rewrite_keeps_translated_toc_text_inside_original_markup() {
    let xhtml = r#"<div><p><span class="title">Contents</span></p><p><a href="chapter.xhtml">Chapter</a></p></div>"#;
    let replacements = [(1, "目录".to_string()), (2, "第一章".to_string())]
        .into_iter()
        .collect();

    let rewritten = rewrite_xhtml(xhtml, &replacements).expect("XHTML should rewrite");
    let rewritten = String::from_utf8(rewritten).expect("XHTML should remain UTF-8");

    assert!(rewritten.contains(r#"<span class="title">目录</span>"#));
    assert!(rewritten.contains(r#"<a href="chapter.xhtml">第一章</a>"#));
}

#[test]
fn epub_refill_preserves_resources_and_updates_content_and_navigation() {
    let mut snapshot = snapshot();
    snapshot.project.source_file = "book.epub".to_string();
    snapshot.chapters[0].segments[1].source = "Hello world!".to_string();
    snapshot.chapters[0].segments[1].target = Some("你好, 世界!".to_string());
    snapshot.source_bytes = source_epub();

    let bytes = render_epub(&snapshot).expect("EPUB should render");
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
    let mimetype = archive.by_index(0).expect("mimetype should be first");
    assert_eq!(mimetype.name(), "mimetype");
    assert_eq!(mimetype.compression(), CompressionMethod::Stored);
    drop(mimetype);
    assert_eq!(read_entry(&mut archive, "OEBPS/image.bin"), b"image-bytes");
    assert_eq!(read_entry(&mut archive, "OEBPS/style.css"), b"h1{}");

    let chapter = String::from_utf8(read_entry(&mut archive, "OEBPS/chapter.xhtml"))
        .expect("chapter should be UTF-8");
    assert!(chapter.contains("<h1 id=\"top\">第一章</h1>"));
    assert!(chapter.contains("<p class=\"lead\">你好， 世界！<em></em><img"));
    assert!(!chapter.contains("Hello"));
    let nav = String::from_utf8(read_entry(&mut archive, "OEBPS/nav.xhtml"))
        .expect("nav should be UTF-8");
    assert!(nav.contains(">第一章<span></span></a>"));
    let ncx =
        String::from_utf8(read_entry(&mut archive, "OEBPS/toc.ncx")).expect("NCX should be UTF-8");
    assert!(ncx.contains("<text>第一章</text>"));
    let opf = String::from_utf8(read_entry(&mut archive, "OEBPS/content.opf"))
        .expect("OPF should be UTF-8");
    assert!(opf.contains("<dc:language>zh-CN</dc:language>"));
}

#[test]
fn txt_source_generates_readable_basic_epub() {
    let snapshot = snapshot();
    let bytes = render_epub(&snapshot).expect("basic EPUB should render");
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
    assert!(archive.by_name("META-INF/container.xml").is_ok());
    let chapter = String::from_utf8(read_entry(&mut archive, "OEBPS/chapter-0001.xhtml"))
        .expect("chapter should be UTF-8");
    assert!(chapter.contains("<h1>第一章</h1>"));
    assert!(chapter.contains("<p>你好， 世界！</p>"));
    let nav = String::from_utf8(read_entry(&mut archive, "OEBPS/nav.xhtml"))
        .expect("nav should be UTF-8");
    assert!(nav.contains("chapter-0001.xhtml\">第一章</a>"));
}

#[test]
fn epub_refill_rejects_source_alignment_mismatch() {
    let mut snapshot = snapshot();
    snapshot.project.source_file = "book.epub".to_string();
    snapshot.source_bytes = source_epub();
    let error = render_epub(&snapshot).expect_err("mismatched source must fail");
    assert!(error.contains("EPUB alignment failed"));
}

#[test]
fn epub_refill_handles_chapters_split_across_spine_documents() {
    let mut snapshot = snapshot();
    snapshot.project.source_file = "book.epub".to_string();
    snapshot.chapters[0].segments[1].source = "Hello world!".to_string();
    snapshot.source_bytes = grouped_source_epub();

    let bytes = render_epub(&snapshot).expect("grouped EPUB should render");
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
    let opening = String::from_utf8(read_entry(&mut archive, "OEBPS/opening.xhtml"))
        .expect("opening should be UTF-8");
    let continuation = String::from_utf8(read_entry(&mut archive, "OEBPS/continuation.xhtml"))
        .expect("continuation should be UTF-8");
    assert!(opening.contains("<h1>第一章</h1>"));
    assert!(continuation.contains("<p>你好， 世界！</p>"));
}

fn source_epub() -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .expect("mimetype should start");
    writer
        .write_all(b"application/epub+zip")
        .expect("mimetype should write");
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let entries: [(&str, &[u8]); 7] = [
        (
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
        ),
        (
            "OEBPS/content.opf",
            br#"<?xml version="1.0"?><package xmlns:dc="urn:dc"><metadata><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="css" href="style.css" media-type="text/css"/><item id="image" href="image.bin" media-type="application/octet-stream"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
        ),
        (
            "OEBPS/chapter.xhtml",
            br#"<?xml version="1.0"?><html><body><h1 id="top">Chapter 1</h1><p class="lead">Hello <em>world</em>!<img src="image.bin"/></p></body></html>"#,
        ),
        (
            "OEBPS/nav.xhtml",
            br#"<?xml version="1.0"?><html><body><nav><ol><li><a href="chapter.xhtml#top"><span>Chapter 1</span></a></li></ol></nav></body></html>"#,
        ),
        (
            "OEBPS/toc.ncx",
            br#"<?xml version="1.0"?><ncx><navMap><navPoint><navLabel><text>Chapter 1</text></navLabel><content src="chapter.xhtml#top"/></navPoint></navMap></ncx>"#,
        ),
        ("OEBPS/style.css", b"h1{}"),
        ("OEBPS/image.bin", b"image-bytes"),
    ];
    for (name, contents) in entries {
        writer
            .start_file(name, options)
            .expect("entry should start");
        writer.write_all(contents).expect("entry should write");
    }
    writer.finish().expect("EPUB should finish").into_inner()
}

fn grouped_source_epub() -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .expect("mimetype should start");
    writer
        .write_all(b"application/epub+zip")
        .expect("mimetype should write");
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let entries: [(&str, &[u8]); 5] = [
        (
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
        ),
        (
            "OEBPS/content.opf",
            br#"<?xml version="1.0"?><package><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="opening" href="opening.xhtml" media-type="application/xhtml+xml"/><item id="continuation" href="continuation.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="opening"/><itemref idref="continuation"/></spine></package>"#,
        ),
        (
            "OEBPS/nav.xhtml",
            br#"<?xml version="1.0"?><html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="opening.xhtml">Chapter 1</a></li></ol></nav></body></html>"#,
        ),
        (
            "OEBPS/opening.xhtml",
            br#"<?xml version="1.0"?><html><body><h1>Chapter 1</h1></body></html>"#,
        ),
        (
            "OEBPS/continuation.xhtml",
            br#"<?xml version="1.0"?><html><body><p>Hello world!</p></body></html>"#,
        ),
    ];
    for (name, contents) in entries {
        writer
            .start_file(name, options)
            .expect("entry should start");
        writer.write_all(contents).expect("entry should write");
    }
    writer.finish().expect("EPUB should finish").into_inner()
}

fn read_entry<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>, name: &str) -> Vec<u8> {
    let mut entry = archive.by_name(name).expect("entry should exist");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("entry should read");
    bytes
}
