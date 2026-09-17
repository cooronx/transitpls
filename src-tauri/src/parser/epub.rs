//! EPUB 解析：读取 container/OPF，按 spine 与导航文档构建章节。

use super::archive::{normalize_zip_path, read_zip_entry};
use super::chapter::build_chapter_from_blocks;
use super::opf::{parse_chapter_navigation, parse_opf, parse_rootfile_path};
use super::xhtml::{fallback_xhtml_blocks, parse_xhtml_blocks};
use crate::model::{Chapter, SegmentKind};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use zip::ZipArchive;

pub(super) fn parse_epub(
    path: &Path,
    max_chars: usize,
) -> Result<(String, Vec<Chapter>, String), String> {
    let file = File::open(path).map_err(|error| format!("failed to open EPUB: {error}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("invalid EPUB zip: {error}"))?;
    let container = read_zip_entry(&mut archive, "META-INF/container.xml")?;
    let opf_path = parse_rootfile_path(&container)?;
    let opf = read_zip_entry(&mut archive, &opf_path)?;
    let opf_dir = Path::new(&opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let (title, manifest, spine, navigation_href, ncx_href) = parse_opf(&opf)?;
    // EPUB3 用 nav 文档，EPUB2 用 NCX；两者都统一成「文档路径 -> 章节标题」。
    let navigation = if let Some(href) = navigation_href.or(ncx_href) {
        let path = normalize_zip_path(opf_dir, &href);
        let document = read_zip_entry(&mut archive, &path)?;
        let navigation_dir = Path::new(&path).parent().unwrap_or_else(|| Path::new(""));
        parse_chapter_navigation(&document, href.ends_with(".ncx"))?
            .into_iter()
            .map(|(href, title)| {
                let href = href.split('#').next().unwrap_or(&href);
                (normalize_zip_path(navigation_dir, href), title)
            })
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };

    let mut documents = Vec::new();
    for idref in spine {
        let Some(href) = manifest.get(&idref) else {
            continue;
        };
        let href = href.split('#').next().unwrap_or(href);
        let entry_path = normalize_zip_path(opf_dir, href);
        let html = read_zip_entry(&mut archive, &entry_path)?;
        let mut blocks = parse_xhtml_blocks(&html);
        if blocks.is_empty() {
            blocks = fallback_xhtml_blocks(&html);
        }
        let content = blocks
            .into_iter()
            .filter(|(_, _, text)| !text.trim().is_empty())
            .map(|(_, kind, text)| (kind, text))
            .collect::<Vec<_>>();
        documents.push((entry_path, content));
    }

    let chapters = if navigation.is_empty() {
        // 没有导航时，每个 spine 文档自成一章，用第一个标题块作为章节名。
        documents
            .into_iter()
            .enumerate()
            .map(|(ordinal, (_, content))| {
                let chapter_title = content
                    .iter()
                    .find(|(kind, text)| matches!(kind, SegmentKind::Heading) && !text.is_empty())
                    .map(|(_, text)| text.clone())
                    .unwrap_or_else(|| format!("Chapter {}", ordinal + 1));
                build_chapter_from_blocks(ordinal, chapter_title, content, max_chars)
            })
            .collect::<Vec<_>>()
    } else {
        group_spine_by_navigation(documents, &navigation, max_chars)
    };
    if chapters.is_empty() {
        return Err("EPUB spine contains no readable chapters".to_string());
    }
    Ok((
        title.unwrap_or_else(|| "Untitled".to_string()),
        chapters,
        "epub".to_string(),
    ))
}

/// 按导航标题合并 spine 文档：每个导航项开启一章，直到下一个导航项之前的文档都归入本章。
fn group_spine_by_navigation(
    documents: Vec<(String, Vec<(SegmentKind, String)>)>,
    navigation: &HashMap<String, String>,
    max_chars: usize,
) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut current: Option<(String, Vec<(SegmentKind, String)>)> = None;
    let push_chapter =
        |chapters: &mut Vec<Chapter>, title: String, blocks: Vec<(SegmentKind, String)>| {
            let chapter = build_chapter_from_blocks(chapters.len(), title, blocks, max_chars);
            // 导航中可能包含没有正文的目录项，跳过空章节。
            if !chapter.segments.is_empty() {
                chapters.push(chapter);
            }
        };
    for (path, content) in documents {
        if let Some(title) = navigation.get(&path) {
            if let Some((title, blocks)) = current.take() {
                push_chapter(&mut chapters, title, blocks);
            }
            current = Some((title.clone(), Vec::new()));
        }
        if let Some((_, blocks)) = current.as_mut() {
            blocks.extend(content);
        }
    }
    if let Some((title, blocks)) = current {
        push_chapter(&mut chapters, title, blocks);
    }
    chapters
}
