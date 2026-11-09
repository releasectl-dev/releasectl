//! releasectl — App Store Connect / Google Play の課金カタログを 1 枚の iap.yaml
//! から reconcile する CLI。
//!
//! `plan` は書き込みをせず、`apply` が何を作るかを表示する。鍵の env が無い
//! provider は飛ばすので、持っている鍵の分だけ実行できる。どれも冪等。

use clap::{Parser, Subcommand};
use releasectl_core::{
    Action, Config, JSON_VERSION, Report, RunReport, Status, Summary, exit_code,
};
use releasectl_stores::PROVIDERS;
use releasectl_stores::googleplay::upload::{UploadArgs, upload};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "releasectl",
    version,
    about = "App Store Connect / Google Play の課金カタログを iap.yaml から reconcile する"
)]
struct Cli {
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    /// 課金商品（サブスク / 買い切り）
    Iap {
        #[command(subcommand)]
        command: IapCmd,
    },
    /// Google Play のビルド
    Play {
        #[command(subcommand)]
        command: PlayCmd,
    },
}

#[derive(Subcommand)]
enum IapCmd {
    /// apply が何を作るかを表示する。書き込みはしない
    Plan(Args),
    /// 足りないものを作り、価格と掲載を更新する。冪等で再実行してよい
    Apply(Args),
    /// retire が何を販売停止にするかを表示する。書き込みはしない
    RetirePlan(Args),
    /// 設定した商品を販売停止にする。何も消さず、apply で販売に戻る。
    /// 販売停止の API が無いストア（App Store）は手作業を案内する
    Retire(Args),
}

#[derive(clap::Args)]
struct Args {
    /// desired-state の YAML
    #[arg(short, long, default_value = "iap.yaml")]
    config: PathBuf,
    /// 実行する provider をカンマ区切りで絞る（appstore,googleplay）
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// 出力の形（json は Cloud・CI 向け。人向けの表示は出さない）
    #[arg(short, long, value_enum, default_value_t = Output::Text)]
    output: Output,
    /// 終了コードを 0 = 差分なし / 2 = 差分あり / 1 = エラー にする（terraform と同じ）
    #[arg(long)]
    detailed_exitcode: bool,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum Output {
    Text,
    Json,
}

#[derive(Subcommand)]
enum PlayCmd {
    /// .aab を上げて検証し、edit を捨てる（公開されるものは何も変わらない）
    UploadPlan(UploadCli),
    /// .aab をトラックに上げて commit する
    Upload(UploadCli),
}

#[derive(clap::Args)]
struct UploadCli {
    /// applicationId（例: dev.releasectl.e2e）
    #[arg(long)]
    package: String,
    /// .aab のパス
    #[arg(long)]
    aab: PathBuf,
    /// internal / alpha / beta
    #[arg(long, default_value = "internal")]
    track: String,
    /// completed / draft（公開前のアプリは draft）
    #[arg(long, default_value = "completed")]
    status: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Plan,
    Apply,
    RetirePlan,
    Retire,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Top::Iap { command } => match command {
            IapCmd::Plan(a) => run(&a, Mode::Plan),
            IapCmd::Apply(a) => run(&a, Mode::Apply),
            IapCmd::RetirePlan(a) => run(&a, Mode::RetirePlan),
            IapCmd::Retire(a) => run(&a, Mode::Retire),
        },
        Top::Play { command } => match command {
            PlayCmd::UploadPlan(a) => play_upload(&a, false),
            PlayCmd::Upload(a) => play_upload(&a, true),
        },
    }
}

fn run(args: &Args, mode: Mode) -> ExitCode {
    let json = args.output == Output::Json;
    // json のときは標準出力に JSON 1 つだけを出す。人向けの行はここを通して捨てる。
    let say = |line: String| {
        if !json {
            println!("{line}");
        }
    };
    let cfg = match Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {e}");
            return ExitCode::from(1);
        }
    };
    let selected: Option<HashSet<&str>> = {
        let s: HashSet<&str> = args
            .only
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        (!s.is_empty()).then_some(s)
    };

    let app = &cfg.app_name;
    say(match mode {
        Mode::Plan => format!("PLAN — no changes will be made ({app:?})"),
        Mode::Apply => format!("APPLY — creating real store resources for {app:?}"),
        Mode::RetirePlan => format!("RETIRE PLAN — no changes will be made ({app:?})"),
        Mode::Retire => {
            format!("RETIRE — taking real store products OFF SALE for {app:?} (nothing is deleted)")
        }
    });
    let apply = matches!(mode, Mode::Apply | Mode::Retire);

    let mut all: Vec<Action> = Vec::new();
    let mut had_fatal = false;
    for (name, build) in PROVIDERS {
        if selected.as_ref().is_some_and(|s| !s.contains(name)) {
            continue;
        }
        say(format!("\n=== {name} ==="));
        let mut report = Report::new(name);
        let rec = match build(&cfg) {
            Ok(r) => r,
            Err(reason) => {
                say(format!("  skipped: {reason}"));
                report.add("provider", "", Status::Skip, reason);
                all.extend(report.actions);
                continue;
            }
        };
        let res = match mode {
            Mode::Plan | Mode::Apply => rec.reconcile(apply, &mut report),
            Mode::RetirePlan | Mode::Retire => rec.retire(apply, &mut report),
        };
        if !json {
            print_actions(&report);
        }
        if let Err(e) = res {
            eprintln!("  fatal: {e}");
            had_fatal = true;
            report.add("provider", "", Status::Error, e);
        }
        all.extend(report.actions);
    }

    match mode {
        Mode::Plan => {
            say("\nplan only — run `releasectl iap apply` to create the missing resources.".into())
        }
        Mode::RetirePlan => {
            say("\nplan only — run `releasectl iap retire` to take the products off sale.".into())
        }
        _ => {}
    }
    if json {
        let rep = RunReport {
            version: JSON_VERSION,
            app,
            command: match mode {
                Mode::Plan => "plan",
                Mode::Apply => "apply",
                Mode::RetirePlan => "retire-plan",
                Mode::Retire => "retire",
            },
            summary: Summary::of(&all),
            actions: &all,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&rep).expect("serialize report")
        );
    }
    ExitCode::from(exit_code(&all, had_fatal, args.detailed_exitcode))
}

fn print_actions(report: &Report) {
    if report.actions.is_empty() {
        println!("  (nothing to do)");
        return;
    }
    for a in &report.actions {
        println!("{}", a.render());
    }
}

fn play_upload(a: &UploadCli, apply: bool) -> ExitCode {
    let data = match std::fs::read(&a.aab) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: read {}: {e}", a.aab.display());
            return ExitCode::from(1);
        }
    };
    let label = a.aab.display().to_string();
    let args = UploadArgs {
        package: &a.package,
        aab: &data,
        aab_label: &label,
        track: &a.track,
        status: &a.status,
    };
    match upload(&args, apply, &mut |line| println!("{line}")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
