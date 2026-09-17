//! `terms` 命令：列出术语、查看冲突并人工裁定译名。

use super::args::{TermsArgs, TermsCommand, TermsResolveArgs};
use super::status::load_project_args;
use crate::model::ProjectState;
use crate::polish;
use crate::state;
use crate::terms::TermStore;
use std::path::Path;

pub(super) fn run(args: TermsArgs, state_dir: &Path) -> Result<i32, String> {
    match args.command {
        TermsCommand::List(selector) => {
            let project = load_project_args(state_dir, &selector)?;
            let store = term_store(state_dir, &project)?;
            println!("source\ttarget\ttype\tstatus\taliases");
            for term in store.list()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    term.source,
                    term.target,
                    term.term_type,
                    term.status,
                    term.aliases.join(", ")
                );
            }
            Ok(0)
        }
        TermsCommand::Conflicts(selector) => {
            let project = load_project_args(state_dir, &selector)?;
            let store = term_store(state_dir, &project)?;
            println!("source\tcandidate\tchapter");
            for conflict in store.conflicts()? {
                println!(
                    "{}\t{}\t{}",
                    conflict.source, conflict.target, conflict.chapter
                );
            }
            for conflict in store.alias_conflicts()? {
                println!(
                    "{}\talias shared by {} and {}\t-",
                    conflict.alias, conflict.first_source, conflict.second_source
                );
            }
            Ok(0)
        }
        TermsCommand::Resolve(args) => resolve(state_dir, args),
    }
}

fn resolve(state_dir: &Path, args: TermsResolveArgs) -> Result<i32, String> {
    let (project, source, target) = if let Some(id) = args.project {
        if args.values.len() != 2 {
            return Err("terms resolve --project expects SOURCE and TARGET arguments".to_string());
        }
        (
            state::load_project(state_dir, &id)?,
            &args.values[0],
            &args.values[1],
        )
    } else {
        if args.values.len() != 3 {
            return Err("terms resolve expects INPUT, SOURCE, and TARGET arguments".to_string());
        }
        (
            state::load_for_source(state_dir, std::path::Path::new(&args.values[0]))?,
            &args.values[1],
            &args.values[2],
        )
    };
    let store = term_store(state_dir, &project)?;
    store.resolve(source, target)?;
    // 术语变化会让旧润色快照失效。
    polish::invalidate_round(state_dir, &project.id)?;
    state::append_log(
        state_dir,
        &project,
        "term_resolved",
        serde_json::json!({ "source": source, "target": target }),
    )?;
    println!("resolved {source} -> {target}");
    Ok(0)
}

fn term_store(state_dir: &Path, project: &ProjectState) -> Result<TermStore, String> {
    TermStore::open(state::project_dir(state_dir, &project.id).join("terms.db"))
}
