//! parser 模块的单元测试：长文本切分、OPF/封面解析、XHTML 块提取与 EPUB 导航分章。

use super::opf::{parse_cover_reference, parse_opf};
use super::{extract_epub_cover, parse_document, parse_xhtml_blocks, split_long_text};
use crate::model::SegmentKind;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn split_long_text_respects_unicode_character_limit() {
    let chunks = split_long_text("第一句。第二句。第三句。", 5);
    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 5));
    assert_eq!(chunks.join(""), "第一句。第二句。第三句。");
}

#[test]
fn parses_self_closing_opf_manifest_items_in_spine_order() {
    let xml = r#"
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata><dc:title>Book</dc:title></metadata>
              <manifest>
                <item id="second" href="second.xhtml" media-type="application/xhtml+xml"/>
                <item id="first" href="first.xhtml" media-type="application/xhtml+xml"/>
              </manifest>
              <spine><itemref idref="first"/><itemref idref="second"/></spine>
            </package>
        "#;
    let (title, manifest, spine, navigation, _) = parse_opf(xml).expect("valid OPF");
    assert_eq!(title.as_deref(), Some("Book"));
    assert_eq!(
        manifest.get("first").map(String::as_str),
        Some("first.xhtml")
    );
    assert_eq!(spine, vec!["first", "second"]);
    assert!(navigation.is_none());
}

#[test]
fn normalizes_relative_epub_paths() {
    assert_eq!(
        super::normalize_zip_path(Path::new("OEBPS"), "../Text/chapter.xhtml"),
        "Text/chapter.xhtml"
    );
}

#[test]
fn preserves_inline_punctuation_when_collecting_xhtml_text() {
    let blocks = parse_xhtml_blocks("<p>Hello <em>world</em>.</p>");
    assert_eq!(blocks[0].1, SegmentKind::Paragraph);
    assert_eq!(blocks[0].2, "Hello world.");
}

#[test]
fn accepts_prefixed_opf_namespaces() {
    let xml = r#"
            <opf:package xmlns:opf="urn:opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
              <opf:metadata><dc:title>Book</dc:title></opf:metadata>
              <opf:manifest><opf:item id="one" href="one.xhtml" media-type="application/xhtml+xml"/></opf:manifest>
              <opf:spine><opf:itemref idref="one"/></opf:spine>
            </opf:package>
        "#;
    let (title, manifest, spine, navigation, _) = parse_opf(xml).expect("valid prefixed OPF");
    assert_eq!(title.as_deref(), Some("Book"));
    assert!(manifest.contains_key("one"));
    assert_eq!(spine, vec!["one"]);
    assert!(navigation.is_none());
}

#[test]
fn recognizes_epub2_cover_metadata() {
    let xml = r#"
            <package>
              <metadata><meta name="cover" content="legacy-cover"/></metadata>
              <manifest>
                <item id="legacy-cover" href="images/front.png" media-type="image/png"/>
              </manifest>
            </package>
        "#;

    let cover = parse_cover_reference(xml)
        .expect("valid OPF")
        .expect("cover should be present");

    assert_eq!(
        cover,
        ("images/front.png".to_string(), "image/png".to_string())
    );
}

#[test]
fn extracts_epub3_cover_image() {
    let root = std::env::temp_dir().join(format!(
        "transitpls-parser-cover-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("test")
            .replace(':', "_")
    ));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let path = root.join("book.epub");
    let file = File::create(&path).expect("fixture EPUB should be created");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, contents) in [
        (
            "META-INF/container.xml",
            br#"<container><rootfiles><rootfile full-path="OPS/package.opf"/></rootfiles></container>"#.as_slice(),
        ),
        (
            "OPS/package.opf",
            br#"<package><manifest><item id="cover" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/></manifest></package>"#.as_slice(),
        ),
        ("OPS/images/cover.jpg", &[0xff, 0xd8, 0xff, 0xd9]),
    ] {
        writer
            .start_file(name, options)
            .expect("fixture entry should start");
        writer
            .write_all(contents)
            .expect("fixture entry should be written");
    }
    writer.finish().expect("fixture EPUB should finish");

    let cover = extract_epub_cover(&path)
        .expect("EPUB should parse")
        .expect("cover should be extracted");

    assert_eq!(cover.media_type, "image/jpeg");
    assert_eq!(cover.bytes, [0xff, 0xd8, 0xff, 0xd9]);
    fs::remove_dir_all(root).expect("fixture should be removed");
}

#[test]
fn uses_epub_navigation_to_group_spine_documents() {
    let root = std::env::temp_dir().join(format!(
        "transitpls-parser-nav-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("test")
            .replace(':', "_")
    ));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let path = root.join("book.epub");
    let file = File::create(&path).expect("fixture EPUB should be created");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, contents) in [
        (
            "META-INF/container.xml",
            r#"<container><rootfiles><rootfile full-path="OEBPS/package.opf"/></rootfiles></container>"#,
        ),
        (
            "OEBPS/package.opf",
            r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Book</dc:title></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="first-start" href="first-start.xhtml" media-type="application/xhtml+xml"/><item id="first-body" href="first-body.xhtml" media-type="application/xhtml+xml"/><item id="second" href="second.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="first-start"/><itemref idref="first-body"/><itemref idref="second"/></spine></package>"#,
        ),
        (
            "OEBPS/nav.xhtml",
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="first-start.xhtml#one">First chapter</a></li><li><a href="second.xhtml#two">Second chapter</a></li></ol></nav></body></html>"#,
        ),
        (
            "OEBPS/first-start.xhtml",
            r#"<html><head><title>Book</title></head><body><p id="one">First opening</p></body></html>"#,
        ),
        (
            "OEBPS/first-body.xhtml",
            r#"<html><head><title>Book</title></head><body><p>First continuation</p></body></html>"#,
        ),
        (
            "OEBPS/second.xhtml",
            r#"<html><head><title>Book</title></head><body><p id="two">Second body</p></body></html>"#,
        ),
    ] {
        writer
            .start_file(name, options)
            .expect("fixture entry should start");
        writer
            .write_all(contents.as_bytes())
            .expect("fixture entry should be written");
    }
    writer.finish().expect("fixture EPUB should finish");

    let document = parse_document(&path, Some("en"), 1_200).expect("EPUB should parse");

    assert_eq!(document.chapters.len(), 2);
    assert_eq!(document.chapters[0].title, "First chapter");
    assert_eq!(document.chapters[1].title, "Second chapter");
    assert!(document.chapters[0]
        .segments
        .iter()
        .any(|segment| segment.source == "First continuation"));
    assert!(document
        .chapters
        .iter()
        .flat_map(|chapter| &chapter.segments)
        .all(|segment| segment.source != "Book"));
    fs::remove_dir_all(root).expect("fixture should be removed");
}

#[test]
fn uses_epub2_ncx_to_group_spine_documents() {
    let root = std::env::temp_dir().join(format!("transitpls-parser-ncx-{}", std::process::id()));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let path = root.join("book.epub");
    let file = File::create(&path).expect("fixture EPUB should be created");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, contents) in [
        (
            "META-INF/container.xml",
            r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
        ),
        (
            "content.opf",
            r#"<package><metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Book</dc:title></metadata><manifest><item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/><item id="toc" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="toc-page" href="toc-page.xhtml" media-type="application/xhtml+xml"/><item id="first" href="first.xhtml" media-type="application/xhtml+xml"/><item id="first-body" href="first-body.xhtml" media-type="application/xhtml+xml"/><item id="second" href="second.xhtml" media-type="application/xhtml+xml"/></manifest><spine toc="toc"><itemref idref="cover"/><itemref idref="toc-page"/><itemref idref="first"/><itemref idref="first-body"/><itemref idref="second"/></spine></package>"#,
        ),
        (
            "toc.ncx",
            r#"<ncx><navMap><navPoint><navLabel><text>目次</text></navLabel><content src="toc-page.xhtml"/></navPoint><navPoint><navLabel><text>First</text></navLabel><content src="first.xhtml"/></navPoint><navPoint><navLabel><text>Second</text></navLabel><content src="second.xhtml"/></navPoint></navMap></ncx>"#,
        ),
        ("cover.xhtml", "<html><body><p>Cover</p></body></html>"),
        (
            "toc-page.xhtml",
            "<html><body><p>Contents</p></body></html>",
        ),
        (
            "first.xhtml",
            "<html><body><p>First title</p></body></html>",
        ),
        (
            "first-body.xhtml",
            "<html><body><p>First body</p></body></html>",
        ),
        (
            "second.xhtml",
            "<html><body><p>Second body</p></body></html>",
        ),
    ] {
        writer
            .start_file(name, options)
            .expect("fixture entry should start");
        writer
            .write_all(contents.as_bytes())
            .expect("fixture entry should be written");
    }
    writer.finish().expect("fixture EPUB should finish");

    let document = parse_document(&path, Some("en"), 1_200).expect("EPUB should parse");

    assert_eq!(document.chapters.len(), 2);
    assert_eq!(document.chapters[0].title, "First");
    assert_eq!(document.chapters[1].title, "Second");
    assert!(document.chapters[0]
        .segments
        .iter()
        .any(|segment| segment.source == "First body"));
    fs::remove_dir_all(root).expect("fixture should be removed");
}
