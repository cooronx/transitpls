use super::{copy_epub_with_replacements, is_xhtml, read_entry_string, read_epub_package};
use crate::export::ExportLayout;
use crate::parser;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use zip::ZipArchive;

pub(in crate::export) fn apply_layout(
    bytes: Vec<u8>,
    layout: ExportLayout,
) -> Result<Vec<u8>, String> {
    let (mode, direction) = match layout {
        ExportLayout::Preserve => return Ok(bytes),
        ExportLayout::Vertical => ("vertical-rl", "rtl"),
        ExportLayout::Horizontal => ("horizontal-tb", "ltr"),
    };
    let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| e.to_string())?;
    let package = read_epub_package(&mut archive)?;
    let dir = Path::new(&package.opf_path)
        .parent()
        .unwrap_or(Path::new(""));
    let mut replacements = HashMap::new();
    let opf = read_entry_string(&mut archive, &package.opf_path)?;
    replacements.insert(
        package.opf_path.clone(),
        rewrite_layout(&opf, mode, direction, true)?,
    );
    for item in package.manifest.values().filter(|item| is_xhtml(item)) {
        let path = parser::normalize_zip_path(dir, &item.href);
        let xhtml = read_entry_string(&mut archive, &path)?;
        replacements.insert(path, rewrite_layout(&xhtml, mode, direction, false)?);
    }
    copy_epub_with_replacements(archive, &replacements)
}

fn rewrite_layout(xml: &str, mode: &str, direction: &str, opf: bool) -> Result<Vec<u8>, String> {
    let declarations = format!("writing-mode: {mode} !important; -webkit-writing-mode: {mode} !important; -epub-writing-mode: {mode} !important;");
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::new());
    let mut in_body = false;
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                let local = tag.local_name();
                if !opf && local.as_ref() == "body" {
                    in_body = !empty;
                }
                let attribute = if opf && local.as_ref() == "spine" {
                    Some(("page-progression-direction", direction))
                } else if !opf && (in_body || matches!(local.as_ref(), "html" | "body")) {
                    Some(("style", declarations.as_str()))
                } else {
                    None
                };
                let tag = if let Some((key, value)) = attribute {
                    let mut copy = BytesStart::new(tag.name().as_ref().to_owned());
                    let mut previous = String::new();
                    for attr in tag.attributes() {
                        let attr = attr.map_err(|e| e.to_string())?;
                        if attr.key.as_ref() == key {
                            previous = attr
                                .normalized_value(XmlVersion::Implicit1_0)
                                .map_err(|e| e.to_string())?
                                .into_owned();
                        } else {
                            copy.push_attribute(attr);
                        }
                    }
                    // Override even descendant and inline !important rules from the original book.
                    let value = if key == "style" {
                        format!("{previous}; {value}")
                    } else {
                        value.to_owned()
                    };
                    copy.push_attribute((key, value.as_str()));
                    copy
                } else {
                    tag
                };
                writer
                    .write_event(if empty {
                        Event::Empty(tag)
                    } else {
                        Event::Start(tag)
                    })
                    .map_err(|e| e.to_string())?;
            }
            Event::End(tag) => {
                if tag.local_name().as_ref() == "body" {
                    in_body = false;
                }
                writer
                    .write_event(Event::End(tag))
                    .map_err(|e| e.to_string())?;
            }
            Event::Eof => break,
            event => writer.write_event(event).map_err(|e| e.to_string())?,
        }
    }
    Ok(writer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_inline_layout_and_existing_spine_direction() {
        let xhtml = r#"<h:html xmlns:h="http://www.w3.org/1999/xhtml"><h:head/><h:body style="writing-mode:vertical-rl!important"><h:p style="color:red">正文</h:p></h:body></h:html>"#;
        let output =
            String::from_utf8(rewrite_layout(xhtml, "horizontal-tb", "ltr", false).unwrap())
                .unwrap();
        assert!(output.contains("<h:html xmlns:h=\"http://www.w3.org/1999/xhtml\" style=\"; writing-mode: horizontal-tb !important;"));
        assert!(output.contains("color:red; writing-mode: horizontal-tb !important;"));
        assert!(output.contains(
            "writing-mode:vertical-rl!important; writing-mode: horizontal-tb !important;"
        ));
        let opf = r#"<package><spine page-progression-direction="rtl"><itemref idref="second"/><itemref idref="first"/></spine></package>"#;
        let output =
            String::from_utf8(rewrite_layout(opf, "horizontal-tb", "ltr", true).unwrap()).unwrap();
        assert_eq!(output, opf.replace("\"rtl\"", "\"ltr\""));
        let options: crate::export::ExportOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(options.layout, ExportLayout::Preserve);
        assert!(
            serde_json::from_str::<crate::export::ExportOptions>(r#"{"layout":"invalid"}"#)
                .is_err()
        );
    }
}
