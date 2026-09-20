//! export 模块的单元测试：标点规范化、TXT 渲染与 EPUB 回填/生成。

use super::epub::rewrite_xhtml;
use super::punctuation::normalize_chinese_punctuation;
use super::{render_epub, render_txt, ExportOptions, ExportOrder};
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
    let rendered = render_txt(&snapshot(), Default::default()).expect("TXT should render");
    assert_eq!(rendered, "第一章\n\n你好， 世界！\n");
}

#[test]
fn txt_export_rejects_incomplete_translation() {
    let mut snapshot = snapshot();
    snapshot.chapters[0].segments[1].target = None;
    let error =
        render_txt(&snapshot, Default::default()).expect_err("incomplete export should fail");
    assert!(error.contains("empty translation"));
}

fn bilingual(order: ExportOrder) -> ExportOptions {
    ExportOptions {
        bilingual: true,
        order: Some(order),
    }
}

#[test]
fn bilingual_txt_restores_paragraphs_in_both_formats_without_mutation() {
    let mut snapshot = snapshot();
    let source = "Hello, world! Again?";
    snapshot.source_bytes =
        format!("\u{feff}Chapter 1\r\n\r\n{source}\r\n\r\n{source}\r\n\r\n相同！").into_bytes();
    snapshot.project.max_segment_chars = 13;
    let template = snapshot.chapters[0].segments[1].clone();
    snapshot.chapters[0].segments = crate::parser::split_long_text(source, 13)
        .into_iter()
        .cycle()
        .take(4)
        .chain(["相同！".to_string(), String::new()])
        .enumerate()
        .map(|(ordinal, source)| Segment {
            ordinal,
            source,
            target: Some(
                [
                    "你好, 世界!",
                    "再来?",
                    "第二次!",
                    "再来?",
                    "相同!",
                    "补充译文",
                ][ordinal]
                    .to_string(),
            ),
            target_before_polish: Some("旧稿".to_string()),
            ..template.clone()
        })
        .collect();
    let before = serde_json::to_value(&snapshot.chapters).unwrap();
    let target_first = bilingual(ExportOrder::TargetFirst);
    let rendered = render_txt(&snapshot, target_first).unwrap();
    assert_eq!(rendered, format!("第一章\n\n你好， 世界！再来？\n{source}\n\n第二次！再来？\n{source}\n\n相同！\n\n补充译文\n"));
    let source_first = render_txt(&snapshot, bilingual(ExportOrder::SourceFirst)).unwrap();
    assert!(source_first.contains(&format!("{source}\n你好， 世界！再来？")));
    for order in [ExportOrder::TargetFirst, ExportOrder::SourceFirst] {
        let mut archive = ZipArchive::new(Cursor::new(
            render_epub(&snapshot, bilingual(order)).unwrap(),
        ))
        .unwrap();
        let chapter =
            String::from_utf8(read_entry(&mut archive, "OEBPS/chapter-0001.xhtml")).unwrap();
        assert_eq!(chapter.matches("data-transitpls-source=\"\"").count(), 2);
        assert!(chapter.contains("prefers-color-scheme: dark"));
        assert_eq!(chapter.matches("相同！").count(), 1);
        assert_eq!(
            chapter.find(source).unwrap() < chapter.find("你好， 世界！").unwrap(),
            order == ExportOrder::SourceFirst
        );
    }
    assert_eq!(serde_json::to_value(&snapshot.chapters).unwrap(), before);
    snapshot.chapters[0].segments[2].source = "wrong source".to_string();
    assert!(render_txt(&snapshot, target_first)
        .unwrap_err()
        .contains("TXT alignment failed"));
    assert!(render_epub(&snapshot, target_first)
        .unwrap_err()
        .contains("TXT alignment failed"));
}

#[test]
fn export_options_validate_order_and_keep_default_paths_distinct() {
    use super::{default_output_path, ExportFormat};
    use std::path::Path;
    let options = bilingual(ExportOrder::TargetFirst);
    assert_eq!(
        default_output_path(Path::new("book.txt"), ExportFormat::Txt, options),
        Path::new("output/book.zh-bi.txt")
    );
    assert_eq!(
        default_output_path(
            Path::new("book.txt"),
            ExportFormat::Epub,
            Default::default()
        ),
        Path::new("output/book.zh.epub")
    );
    let invalid = ExportOptions {
        bilingual: false,
        order: Some(ExportOrder::SourceFirst),
    };
    assert!(render_txt(&snapshot(), invalid)
        .unwrap_err()
        .contains("requires bilingual"));
    assert!(
        serde_json::from_str::<ExportOptions>(r#"{"bilingual":true,"order":"sideways"}"#).is_err()
    );
}

#[test]
fn atomic_export_replaces_existing_files_and_preserves_failed_destinations() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/atomic-export-fixture");
    std::fs::create_dir_all(&dir).unwrap();
    let output = dir.join("book.txt");
    super::write_atomic(&output, b"old").unwrap();
    super::write_atomic(&output, b"complete new export").unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"complete new export");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&output)
            .unwrap();
        assert!(super::write_atomic(&output, b"failed export").is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"complete new export");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        drop(locked);
    }
    std::fs::remove_dir_all(&dir).unwrap();
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

    let bytes = render_epub(&snapshot, Default::default()).expect("EPUB should render");
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
    let bytes = render_epub(&snapshot, Default::default()).expect("basic EPUB should render");
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
    let error =
        render_epub(&snapshot, Default::default()).expect_err("mismatched source must fail");
    assert!(error.contains("EPUB alignment failed"));
}

#[test]
fn epub_refill_handles_chapters_split_across_spine_documents() {
    let mut snapshot = snapshot();
    snapshot.project.source_file = "book.epub".to_string();
    snapshot.chapters[0].segments[1].source = "Hello world!".to_string();
    snapshot.source_bytes = grouped_source_epub();

    let bytes = render_epub(&snapshot, Default::default()).expect("grouped EPUB should render");
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
    let opening = String::from_utf8(read_entry(&mut archive, "OEBPS/opening.xhtml"))
        .expect("opening should be UTF-8");
    let continuation = String::from_utf8(read_entry(&mut archive, "OEBPS/continuation.xhtml"))
        .expect("continuation should be UTF-8");
    assert!(opening.contains("<h1>第一章</h1>"));
    assert!(continuation.contains("<p>你好， 世界！</p>"));
}

#[test]
fn bilingual_epub_preserves_ruby_resources_and_cross_document_footnotes() {
    let fixture_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/export-fixtures");
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let source = fixture_dir.join("bilingual-source.epub");
    std::fs::write(&source, bilingual_source_epub()).unwrap();
    let document = crate::parser::parse_document(&source, Some("ja"), 12).unwrap();
    let mut snapshot = snapshot();
    snapshot.project.source_file = "bilingual-source.epub".into();
    snapshot.project.source_language = "ja".into();
    snapshot.project.max_segment_chars = 12;
    snapshot.source_bytes = std::fs::read(&source).unwrap();
    snapshot.chapters = document.chapters;
    snapshot.project.chapters_total = snapshot.chapters.len();
    for (index, chapter) in snapshot.chapters.iter_mut().enumerate() {
        chapter.target_title = Some(format!("第{}章", index + 1));
        for segment in &mut chapter.segments {
            segment.status = ItemStatus::Translated;
            segment.target = Some(if segment.kind == SegmentKind::Heading {
                chapter.target_title.clone().unwrap()
            } else {
                format!("译文{} < & >!", segment.ordinal)
            });
        }
    }
    let before = serde_json::to_value(&snapshot.chapters).unwrap();
    for order in [ExportOrder::TargetFirst, ExportOrder::SourceFirst] {
        let bytes = render_epub(&snapshot, bilingual(order)).unwrap();
        std::fs::write(
            fixture_dir.join(format!("bilingual-{order:?}.epub")),
            &bytes,
        )
        .unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            read_entry(&mut archive, "OEBPS/cover.svg"),
            COVER_SVG.as_bytes()
        );
        assert_eq!(archive.len(), 8);
        assert_epub_links_and_structure(&mut archive);
        let main = String::from_utf8(read_entry(&mut archive, "OEBPS/main.xhtml")).unwrap();
        let notes = String::from_utf8(read_entry(&mut archive, "OEBPS/notes.xhtml")).unwrap();
        assert_eq!(main.matches("<img ").count(), 1);
        assert_eq!(main.matches("<ruby>").count(), 1);
        assert!(main.contains("<ruby>彼女<rp>（</rp><rt>かのじょ</rt><rp>）</rp></ruby>"));
        assert!(main.contains("<h1 id=\"top\"><span lang=\"zh-CN\">第1章</span></h1>"));
        assert!(main.contains("href=\"notes.xhtml#transitpls-source-"));
        assert!(main.contains("href=\"appendix.xhtml#static-note\""));
        assert!(notes.contains("href=\"main.xhtml#transitpls-source-"));
        assert!(main.contains(" &lt; &amp; &gt;！"));
        assert!(main.contains("<ul><li>"));
        assert!(main.contains("<blockquote><p>"));
        assert_eq!(
            main.find("<ruby>").unwrap() < main.find("译文1").unwrap(),
            order == ExportOrder::SourceFirst
        );
        let txt = render_txt(&snapshot, bilingual(order)).unwrap();
        assert_eq!(txt.matches("リスト").count(), 1);
        assert!(!txt.contains("<ruby>"));
    }
    assert_eq!(serde_json::to_value(&snapshot.chapters).unwrap(), before);
    snapshot.chapters[0].segments[1].source = "different".into();
    assert!(render_epub(&snapshot, bilingual(ExportOrder::TargetFirst))
        .unwrap_err()
        .contains("alignment failed"));
}

fn assert_epub_links_and_structure(archive: &mut ZipArchive<Cursor<Vec<u8>>>) {
    use quick_xml::{events::Event, Reader, XmlVersion};
    use std::collections::{HashMap, HashSet};
    let paths = archive
        .file_names()
        .filter(|p| p.ends_with(".xhtml"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut ids = HashMap::new();
    let mut links = Vec::new();
    for path in &paths {
        let xml = String::from_utf8(read_entry(archive, path)).unwrap();
        let mut reader = Reader::from_str(&xml);
        let mut stack = Vec::new();
        let mut anchors = HashSet::new();
        loop {
            let event = reader.read_event().expect("output must be well-formed XML");
            let empty = matches!(event, Event::Empty(_));
            match event {
                Event::Start(e) | Event::Empty(e) => {
                    let name = e.name().as_ref().to_string();
                    if matches!(name.as_str(), "p" | "li" | "blockquote" | "ul" | "div") {
                        assert!(
                            !stack.iter().any(|name| name == "span"),
                            "block nested inside span"
                        );
                    }
                    if name == "li" {
                        assert!(stack.last().is_some_and(|p| p == "ul" || p == "ol"));
                    }
                    let mut local = HashSet::new();
                    for attribute in e.attributes() {
                        let attribute = attribute.unwrap();
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .unwrap()
                            .into_owned();
                        match attribute.key.as_ref() {
                            "id" | "name" if local.insert(value.clone()) => {
                                assert!(anchors.insert(value), "duplicate output anchor");
                            }
                            "href" => links.push((path.clone(), value)),
                            _ => {}
                        }
                    }
                    if !empty {
                        stack.push(name);
                    }
                }
                Event::End(_) => {
                    stack.pop().unwrap();
                }
                Event::Eof => {
                    assert!(stack.is_empty());
                    break;
                }
                _ => {}
            }
        }
        ids.insert(path.clone(), anchors);
    }
    for (path, href) in links {
        if href.contains("://") {
            continue;
        }
        let (file, anchor) = href
            .split_once('#')
            .map(|(f, a)| (f, Some(a)))
            .unwrap_or((&href, None));
        let target = if file.is_empty() {
            path.clone()
        } else {
            crate::parser::normalize_zip_path(std::path::Path::new(&path).parent().unwrap(), file)
        };
        assert!(
            archive.by_name(&target).is_ok(),
            "missing link target {href}"
        );
        if let Some(anchor) = anchor {
            assert!(ids[&target].contains(anchor), "broken link {href}");
        }
    }
}

const COVER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="220" viewBox="0 0 160 220"><rect width="160" height="220" fill="#a9bed6"/><text x="20" y="110" font-size="24">Bilingual</text></svg>"##;

fn bilingual_source_epub() -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let entries = [
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ),
        (
            "OEBPS/content.opf",
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="book"><metadata><dc:identifier id="book">bilingual-test</dc:identifier><dc:title>Bilingual sample</dc:title><dc:language>ja</dc:language><meta property="dcterms:modified">2026-09-20T00:00:00Z</meta></metadata><manifest><item id="main" href="main.xhtml" media-type="application/xhtml+xml"/><item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/><item id="appendix" href="appendix.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="cover" href="cover.svg" media-type="image/svg+xml" properties="cover-image"/></manifest><spine><itemref idref="main"/><itemref idref="notes"/></spine></package>"#,
        ),
        (
            "OEBPS/main.xhtml",
            r##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="ja"><head><title>Chapter 1</title><style>body { line-height: 1.6; } .lead { color: #224; }</style></head><body><h1 id="top">Chapter 1</h1><div id="transitpls-source-1"><p class="lead" id="opening"><ruby>彼女<rp>（</rp><rt>かのじょ</rt><rp>）</rp></ruby>は窓を開けた。<a id="ref" name="ref" epub:type="noteref" href="notes.xhtml#note">[1]</a>夜風が部屋に入ってきた。<img id="illustration" src="cover.svg" alt="cover"/></p><ul><li>リストの項目。</li><li><p>入れ子の段落。</p></li></ul><blockquote><p>引用された文章。</p></blockquote><p><a href="appendix.xhtml#static-note">別紙</a>を参照。<a href="#top">章頭</a></p></div></body></html>"##,
        ),
        (
            "OEBPS/notes.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="ja"><head><title>Notes</title></head><body><aside id="note" epub:type="footnote"><p>脚注の説明。<a href="main.xhtml#ref">戻る</a></p></aside></body></html>"#,
        ),
        (
            "OEBPS/appendix.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Appendix</title></head><body><p id="static-note">Untranslated appendix outside the spine.</p></body></html>"#,
        ),
        (
            "OEBPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="main.xhtml#top">Chapter 1</a></li></ol></nav></body></html>"#,
        ),
        ("OEBPS/cover.svg", COVER_SVG),
    ];
    for (path, content) in entries {
        writer
            .start_file(
                path,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
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
