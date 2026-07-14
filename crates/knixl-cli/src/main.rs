//! knixl CLI. Every command is a thin policy over one Plan. Plan::compute is the only
//! thing that inspects the world. Exit codes are stable so CI can branch on them.
//! SPEC-GRADE SKETCH: Ctx::load and the write/report helpers are not written.

use clap::{Parser, Subcommand};
use knixl_lock::{FileState, Plan};

#[derive(Parser)]
#[command(name = "knixl")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Recompute and report; write nothing.
    Plan { #[arg(long)] detailed_exitcode: bool },
    /// CI gate: succeed only if every file is Clean. Never writes, never prompts.
    Check,
    /// Apply. Silent for Stale/Missing; refuses Drifted/skew without the matching flag.
    Generate { #[arg(long)] accept_drift: bool, #[arg(long)] prune: bool },
    /// Version bump: show migration notes + diff, apply on --yes, then bump the lock.
    Upgrade { #[arg(long)] yes: bool },
    /// Print the typed reference for a module node (from schema()).
    Doc { node: String },
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Code {
    Clean = 0,
    Internal = 1,
    // clap owns exit 2 on arg-parse failure; the variant reserves the documented code.
    #[allow(dead_code)]
    Usage = 2,
    Drift = 3,
    NeedsAck = 4,
    Validation = 5,
    RegenPending = 6,
}

/// Precedence, most severe first. Spelled out because severity != numeric order.
/// Validation beats all (cannot trust a plan on invalid input). Drift beats skew
/// (silent overwrite loses human edits). Skew beats plain regen-pending.
fn verdict(plan: &Plan) -> Code {
    if plan.has_validation_errors() { return Code::Validation; }
    if plan.any(FileState::is_drifted) { return Code::Drift; }
    if plan.requires_ack() { return Code::NeedsAck; }
    if plan.any(FileState::is_dirty) { return Code::RegenPending; }
    Code::Clean
}

fn run(cli: Cli, ctx: &Ctx) -> Code {
    // Plan::compute is pure; validation errors ride on the plan (verdict maps them to
    // the Validation exit code), so there is no fallible generation step here.
    let plan = Plan::compute(&ctx.inputs, &ctx.disk, &ctx.lock, &ctx.running);
    if plan.has_validation_errors() {
        report_validation(&plan.validation_errors, cli.json);
        return Code::Validation;
    }

    match cli.cmd {
        Cmd::Plan { detailed_exitcode } => {
            print_plan(&plan, cli.json);
            if detailed_exitcode { verdict(&plan) } else { Code::Clean }
        }

        Cmd::Check => { print_plan(&plan, cli.json); verdict(&plan) }

        Cmd::Generate { accept_drift, prune } => {
            // A version bump must go through `upgrade`, never a side effect of generate.
            if plan.requires_ack() {
                report_skew(&plan, cli.json);
                eprintln!("version skew present: run `knixl upgrade` to review and apply");
                return Code::NeedsAck;
            }
            let mut worst = Code::Clean;
            for f in &plan.files {
                match &f.state {
                    FileState::Clean => {}
                    FileState::Stale { .. } | FileState::Missing { .. } => write_file(f, &plan),
                    FileState::Drifted { .. } if accept_drift => write_file(f, &plan),
                    FileState::Drifted { .. } => { report_taint(f, cli.json); worst = worst.max(Code::Drift); }
                    FileState::Orphaned if prune => delete_file(f),
                    FileState::Orphaned => { note_orphan(f, cli.json); worst = worst.max(Code::RegenPending); }
                }
            }
            // Commit the lock ONLY on a clean apply, so it never lies about disk.
            if worst == Code::Clean { write_lock(&plan.lock_next); }
            worst
        }

        Cmd::Upgrade { yes } => {
            if !plan.requires_ack() && !plan.any(FileState::is_dirty) {
                println!("already up to date");
                return Code::Clean;
            }
            print_migration_notes(&plan); // per (module, version delta)
            print_plan(&plan, cli.json);
            if !yes { eprintln!("re-run with --yes to apply"); return Code::NeedsAck; }
            for f in &plan.files {
                if !matches!(f.state, FileState::Clean) { write_file(f, &plan); }
            }
            write_lock(&plan.lock_next); // bump tool/module/formatter/oracle together
            Code::Clean
        }

        Cmd::Doc { node } => { print_doc(ctx, &node, cli.json); Code::Clean }
    }
}

fn main() {
    // A panic anywhere in planning or applying maps to the Internal exit code (docs/05),
    // so callers get a stable code instead of a raw abort.
    let code = std::panic::catch_unwind(|| run(Cli::parse(), &Ctx::load()))
        .unwrap_or(Code::Internal);
    std::process::exit(code as i32);
}

// ---- everything below: NOT written. Wiring for the next session. ----

struct Ctx {
    inputs: knixl_lock::reconcile::Inputs,
    disk: knixl_lock::reconcile::DiskState,
    lock: knixl_lock::Lock,
    running: knixl_lock::reconcile::Versions,
}
impl Ctx {
    fn load() -> Ctx {
        todo!("discover project root; parse *.kdl inputs; build registry (builtins + modules/); \
               run pipeline to expected output; read on-disk generated + hashes; parse lock; \
               gather running versions")
    }
}

fn print_plan(_p: &Plan, _json: bool) { todo!() }
fn print_migration_notes(_p: &Plan) { todo!() }
fn print_doc(_ctx: &Ctx, _node: &str, _json: bool) { todo!() }
fn report_validation(_errors: &[String], _json: bool) { todo!() }
fn report_skew(_p: &Plan, _json: bool) { todo!() }
fn report_taint(_f: &knixl_lock::FilePlan, _json: bool) { todo!() }
fn note_orphan(_f: &knixl_lock::FilePlan, _json: bool) { todo!() }
fn write_file(_f: &knixl_lock::FilePlan, _p: &Plan) { todo!() }
fn delete_file(_f: &knixl_lock::FilePlan) { todo!() }
fn write_lock(_l: &knixl_lock::Lock) { todo!() }
