//! EPUB 导出：回填原始 EPUB，或从 TXT 快照生成基础 EPUB。

mod bilingual;
mod generate;
mod package;
mod rewrite;

pub(super) use generate::generate_epub;
use package::{is_xhtml, read_entry_string, read_epub_package, EpubPackage};
pub(super) use rewrite::rewrite_xhtml;

use super::punctuation::normalize_chinese_punctuation;
use super::{paragraphs::Paragraph, ExportOptions};
use crate::parser;
use crate::state::ExportSnapshot;
use rewrite::{align_chapter_document, rewrite_nav, rewrite_ncx};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 回填原文 EPUB：按原文重新对齐段落，替换译文与目录标题，其余资源原样复制。
pub(super) fn refill_epub(
    snapshot: &ExportSnapshot,
    options: ExportOptions,
) -> Result<Vec<u8>, String> {
    let mut source = ZipArchive::new(Cursor::new(snapshot.source_bytes.as_slice()))
        .map_err(|error| format!("invalid source EPUB: {error}"))?;
    let package = read_epub_package(&mut source)?;
    let chapters = aligned_chapters(snapshot, &mut source, &package)?;
    let opf_dir = Path::new(&package.opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let navigation_paths = package
        .manifest
        .values()
        .filter(|item| item.properties.split_whitespace().any(|p| p == "nav"))
        .map(|item| parser::normalize_zip_path(opf_dir, &item.href))
        .collect();
    let mut replacements = if options.bilingual {
        bilingual::rewrite_book(&chapters, &mut source, snapshot, options, &navigation_paths)?
    } else {
        let mut replacements = HashMap::new();
        for document in chapters.iter().flatten() {
            let targets = document
                .blocks
                .iter()
                .map(|(index, block)| (*index, block.target.clone()))
                .collect();
            replacements.insert(
                document.path.clone(),
                rewrite_xhtml(&document.xhtml, &targets)?,
            );
        }
        replacements
    };
    let titles_by_path = chapters
        .iter()
        .zip(&snapshot.chapters)
        .filter_map(|(documents, chapter)| {
            documents.first().map(|document| {
                (
                    document.path.clone(),
                    normalize_chinese_punctuation(
                        chapter.target_title.as_deref().expect("validated title"),
                    ),
                )
            })
        })
        .collect();
    for item in package.manifest.values() {
        let entry_path = parser::normalize_zip_path(opf_dir, &item.href);
        if item
            .properties
            .split_whitespace()
            .any(|value| value == "nav")
        {
            let nav = read_entry_string(&mut source, &entry_path)?;
            replacements.insert(
                entry_path.clone(),
                rewrite_nav(&nav, &entry_path, &titles_by_path)?,
            );
        } else if item.media_type == "application/x-dtbncx+xml" {
            let ncx = read_entry_string(&mut source, &entry_path)?;
            replacements.insert(
                entry_path.clone(),
                rewrite_ncx(&ncx, &entry_path, &titles_by_path)?,
            );
        }
    }
    let opf = read_entry_string(&mut source, &package.opf_path)?;
    replacements.insert(
        package.opf_path.clone(),
        rewrite::replace_element_text(&opf, "language", &snapshot.project.target_language)?,
    );
    copy_epub_with_replacements(source, &replacements)
}

struct AlignedDocument {
    path: String,
    xhtml: String,
    blocks: HashMap<usize, Paragraph>,
}

pub(super) fn paragraphs(snapshot: &ExportSnapshot) -> Result<Vec<Vec<Paragraph>>, String> {
    let mut source = ZipArchive::new(Cursor::new(snapshot.source_bytes.as_slice()))
        .map_err(|error| format!("invalid source EPUB: {error}"))?;
    let package = read_epub_package(&mut source)?;
    let chapters = aligned_chapters(snapshot, &mut source, &package)?;
    for document in chapters.iter().flatten() {
        bilingual::validate(document)?;
    }
    Ok(chapters
        .into_iter()
        .map(|documents| {
            documents
                .into_iter()
                .flat_map(|document| {
                    let mut blocks = document.blocks.into_iter().collect::<Vec<_>>();
                    blocks.sort_by_key(|(ordinal, _)| *ordinal);
                    blocks.into_iter().map(|(_, block)| block)
                })
                .collect()
        })
        .collect())
}

fn aligned_chapters(
    snapshot: &ExportSnapshot,
    source: &mut ZipArchive<Cursor<&[u8]>>,
    package: &EpubPackage,
) -> Result<Vec<Vec<AlignedDocument>>, String> {
    let opf_dir = Path::new(&package.opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut document_paths = Vec::new();
    for idref in &package.spine {
        let Some(item) = package.manifest.get(idref) else {
            continue;
        };
        if !is_xhtml(item) {
            continue;
        }
        document_paths.push(parser::normalize_zip_path(
            opf_dir,
            item.href.split('#').next().unwrap_or(&item.href),
        ));
    }
    let chapter_starts = read_chapter_starts(source, package, opf_dir)?;
    let document_count = document_paths.len();
    let mut chapter_paths = group_chapter_paths(document_paths, &chapter_starts);
    if !chapter_starts.is_empty() {
        chapter_paths = retain_readable_groups(source, chapter_paths)?;
    }
    if chapter_paths.len() != snapshot.chapters.len() {
        return Err(format!(
            "EPUB spine has {} readable XHTML documents grouped into {} chapters, but the snapshot has {} chapters",
            document_count,
            chapter_paths.len(),
            snapshot.chapters.len()
        ));
    }

    let mut chapters = Vec::new();
    for ((chapter_index, paths), chapter) in
        chapter_paths.iter().enumerate().zip(&snapshot.chapters)
    {
        let mut segments = chapter.segments.iter().collect::<Vec<_>>();
        segments.sort_by_key(|segment| segment.ordinal);
        let mut segment_index = 0;
        let mut documents = Vec::new();
        for path in paths {
            let xhtml = read_entry_string(source, path)?;
            let block_replacements = align_chapter_document(
                chapter_index,
                &segments,
                &mut segment_index,
                &xhtml,
                snapshot.project.max_segment_chars,
            )?;
            documents.push(AlignedDocument {
                path: path.clone(),
                xhtml,
                blocks: block_replacements,
            });
        }
        if segment_index != segments.len() {
            return Err(format!(
                "EPUB alignment failed in chapter {chapter_index}: {} saved segments were not matched",
                segments.len() - segment_index
            ));
        }
        chapters.push(documents);
    }

    Ok(chapters)
}

/// 读取导航文档中作为章节起点的文档路径集合。
fn read_chapter_starts<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    package: &EpubPackage,
    opf_dir: &Path,
) -> Result<HashSet<String>, String> {
    let navigation = package
        .manifest
        .values()
        .find(|item| {
            item.properties
                .split_whitespace()
                .any(|property| property == "nav")
        })
        .or_else(|| {
            package
                .manifest
                .values()
                .find(|item| item.media_type == "application/x-dtbncx+xml")
        });
    let Some(navigation) = navigation else {
        return Ok(HashSet::new());
    };
    let navigation_path = parser::normalize_zip_path(
        opf_dir,
        navigation
            .href
            .split('#')
            .next()
            .unwrap_or(&navigation.href),
    );
    let navigation_dir = Path::new(&navigation_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let document = read_entry_string(archive, &navigation_path)?;
    Ok(parser::parse_chapter_navigation(
        &document,
        navigation.media_type == "application/x-dtbncx+xml",
    )?
    .into_iter()
    .map(|(href, _)| {
        parser::normalize_zip_path(navigation_dir, href.split('#').next().unwrap_or(&href))
    })
    .collect())
}

/// 按导航起点把 spine 文档分成章节组；没有导航时每个文档自成一章。
fn group_chapter_paths(
    document_paths: Vec<String>,
    chapter_starts: &HashSet<String>,
) -> Vec<Vec<String>> {
    if chapter_starts.is_empty() {
        return document_paths.into_iter().map(|path| vec![path]).collect();
    }
    let mut groups = Vec::new();
    let mut current = None::<Vec<String>>;
    for path in document_paths {
        if chapter_starts.contains(&path) {
            if let Some(paths) = current.take() {
                groups.push(paths);
            }
            current = Some(Vec::new());
        }
        if let Some(paths) = current.as_mut() {
            paths.push(path);
        }
    }
    if let Some(paths) = current {
        groups.push(paths);
    }
    groups
}

/// 丢弃完全没有正文的文档组（如仅含目录页的组），保证分组数与章节数一致。
fn retain_readable_groups<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    groups: Vec<Vec<String>>,
) -> Result<Vec<Vec<String>>, String> {
    let mut readable = Vec::new();
    for group in groups {
        let mut has_content = false;
        for path in &group {
            let xhtml = read_entry_string(archive, path)?;
            if parser::parse_xhtml_blocks(&xhtml)
                .into_iter()
                .any(|(_, _, text)| !text.trim().is_empty())
            {
                has_content = true;
                break;
            }
        }
        if has_content {
            readable.push(group);
        }
    }
    Ok(readable)
}

/// 复制原 EPUB 的所有条目，用替换结果覆盖指定条目；`mimetype` 固定放在首位且不压缩。
fn copy_epub_with_replacements(
    mut source: ZipArchive<Cursor<&[u8]>>,
    replacements: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(|error| format!("failed to write EPUB mimetype: {error}"))?;
    writer
        .write_all(b"application/epub+zip")
        .map_err(|error| format!("failed to write EPUB mimetype: {error}"))?;
    for index in 0..source.len() {
        let entry = source
            .by_index(index)
            .map_err(|error| format!("failed to read source EPUB entry: {error}"))?;
        let name = entry.name().to_string();
        if name == "mimetype" {
            continue;
        }
        if let Some(contents) = replacements.get(&name) {
            let options = entry.options();
            writer
                .start_file(&name, options)
                .map_err(|error| format!("failed to replace EPUB entry '{name}': {error}"))?;
            writer
                .write_all(contents)
                .map_err(|error| format!("failed to replace EPUB entry '{name}': {error}"))?;
        } else {
            writer
                .raw_copy_file(entry)
                .map_err(|error| format!("failed to copy EPUB entry '{name}': {error}"))?;
        }
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("failed to finish EPUB: {error}"))
}
