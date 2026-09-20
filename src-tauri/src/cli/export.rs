//! `export` 命令：渲染并原子写入导出文件。

use super::args::ExportArgs;
use crate::export::{self, ExportFormat, ExportOptions};
use crate::state;
use std::path::Path;

pub(super) fn run(args: ExportArgs, state_dir: &Path) -> Result<i32, String> {
    let format = ExportFormat::from(args.format);
    let options = ExportOptions {
        bilingual: args.bilingual,
        order: args.order,
    };
    options.validate()?;
    let output = args
        .out
        .unwrap_or_else(|| export::default_output_path(&args.input, format, options));
    let snapshot = state::load_export_snapshot(state_dir, &args.input)?;
    let bytes = match format {
        ExportFormat::Txt => export::render_txt(&snapshot, options)?.into_bytes(),
        ExportFormat::Epub => export::render_epub(&snapshot, options)?,
    };
    export::write_atomic(&output, &bytes)?;
    println!("exported {}", output.display());
    Ok(0)
}
