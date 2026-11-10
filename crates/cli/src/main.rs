//! releasectl — App Store Connect / Google Play の課金カタログ（iap.yaml）と掲載情報
//! （metadata.yaml）を reconcile する CLI。
//!
//! `plan` は書き込みをせず、`apply` が何を作るかを表示する。鍵の env が無い
//! provider は飛ばすので、持っている鍵の分だけ実行できる。どれも冪等。

use clap::{Parser, Subcommand};
use releasectl_core::meta::MetaConfig;
use releasectl_core::{
    Action, Config, JSON_VERSION, Report, RunReport, Status, Summary, exit_code,
};
use releasectl_stores::googleplay::upload::{UploadArgs, upload};
use releasectl_stores::meta::META_PROVIDERS;
use releasectl_stores::{PROVIDERS, Reconciler};
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "releasectl",
    version,
    about = "App Store Connect / Google Play の課金カタログと掲載情報を YAML から reconcile する"
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
    /// ストアの掲載情報（テキスト・審査情報・アプリの設定・スクリーンショット・クリエイティブ）
    Meta {
        #[command(subcommand)]
        command: MetaCmd,
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

#[derive(Subcommand)]
enum MetaCmd {
    /// apply が何を変えるかを表示する。公開されるものは何も書かない（Play は edit を開いて捨てる）
    Plan(Args),
    /// 掲載情報を書く。冪等で再実行してよい。審査への提出はしない
    Apply(Args),
}

#[derive(clap::Args)]
struct Args {
    /// desired-state の YAML（既定: iap は iap.yaml、meta は metadata.yaml）
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// 実行する provider をカンマ区切りで絞る（appstore,googleplay）
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// 出力の形（json は Cloud・CI 向け。人向けの表示は出さない）
    #[arg(short, long, value_enum, default_value_t = Output::Text)]
    output: Output,
    /// 終了コードを 0 = 差分なし / 2 = 差分あり / 1 = エラー にする（terraform と同じ）
    #[arg(long)]
    detailed_exitcode: bool,
    /// JSON をこのファイルにも書く（標準出力とは別。GitHub Actions が 1 回の実行で使う）
    #[arg(long)]
    json_out: Option<PathBuf>,
    /// Markdown（PR コメント用）をこのファイルにも書く
    #[arg(long)]
    markdown_out: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum Output {
    Text,
    Json,
    /// PR コメントと job summary 用
    Markdown,
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
enum Kind {
    Iap,
    Meta,
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
            IapCmd::Plan(a) => run(&a, Kind::Iap, Mode::Plan),
            IapCmd::Apply(a) => run(&a, Kind::Iap, Mode::Apply),
            IapCmd::RetirePlan(a) => run(&a, Kind::Iap, Mode::RetirePlan),
            IapCmd::Retire(a) => run(&a, Kind::Iap, Mode::Retire),
        },
        Top::Meta { command } => match command {
            MetaCmd::Plan(a) => run(&a, Kind::Meta, Mode::Plan),
            MetaCmd::Apply(a) => run(&a, Kind::Meta, Mode::Apply),
        },
        Top::Play { command } => match command {
            PlayCmd::UploadPlan(a) => play_upload(&a, false),
            PlayCmd::Upload(a) => play_upload(&a, true),
        },
    }
}

type Build<'a> = Box<dyn Fn() -> Result<Box<dyn Reconciler>, String> + 'a>;
/// 実行順の provider（名前と、その reconciler の作り方）。
type Providers = Vec<(&'static str, Build<'static>)>;

/// 設定を読み、(アプリ名, 実行順の provider) を返す。
fn load(kind: Kind, path: &std::path::Path) -> Result<(String, Providers), String> {
    match kind {
        Kind::Iap => {
            let cfg = std::rc::Rc::new(Config::load(path).map_err(|e| e.to_string())?);
            let app = cfg.app_name.clone();
            let list = PROVIDERS
                .iter()
                .map(|(name, build)| {
                    let (cfg, build) = (cfg.clone(), *build);
                    (*name, Box::new(move || build(&cfg)) as Build)
                })
                .collect();
            Ok((app, list))
        }
        Kind::Meta => {
            let cfg = std::rc::Rc::new(MetaConfig::load(path).map_err(|e| e.to_string())?);
            let app = cfg.app_name.clone();
            let list = META_PROVIDERS
                .iter()
                .map(|(name, build)| {
                    let (cfg, build) = (cfg.clone(), *build);
                    (*name, Box::new(move || build(&cfg)) as Build)
                })
                .collect();
            Ok((app, list))
        }
    }
}

fn run(args: &Args, kind: Kind, mode: Mode) -> ExitCode {
    let json = args.output != Output::Text;
    // json / markdown のときは標準出力にそれ 1 つだけを出す。人向けの行はここを通して捨てる。
    let say = |line: String| {
        if !json {
            println!("{line}");
        }
    };
    let path = args.config.clone().unwrap_or_else(|| {
        PathBuf::from(match kind {
            Kind::Iap => "iap.yaml",
            Kind::Meta => "metadata.yaml",
        })
    });
    let (app, providers) = match load(kind, &path) {
        Ok(v) => v,
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

    let app = &app;
    say(match (kind, mode) {
        (Kind::Meta, Mode::Plan) => format!("PLAN — no published changes will be made ({app:?})"),
        (Kind::Meta, _) => format!("APPLY — writing store listing metadata for {app:?}"),
        (_, Mode::Plan) => format!("PLAN — no changes will be made ({app:?})"),
        (_, Mode::Apply) => format!("APPLY — creating real store resources for {app:?}"),
        (_, Mode::RetirePlan) => format!("RETIRE PLAN — no changes will be made ({app:?})"),
        (_, Mode::Retire) => {
            format!("RETIRE — taking real store products OFF SALE for {app:?} (nothing is deleted)")
        }
    });
    let apply = matches!(mode, Mode::Apply | Mode::Retire);

    let mut all: Vec<Action> = Vec::new();
    let mut had_fatal = false;
    for (name, build) in &providers {
        if selected.as_ref().is_some_and(|s| !s.contains(name)) {
            continue;
        }
        say(format!("\n=== {name} ==="));
        let mut report = Report::new(name);
        let rec = match build() {
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

    match (kind, mode) {
        (Kind::Meta, Mode::Plan) => {
            say("\nplan only — run `releasectl meta apply` to write the metadata.".into())
        }
        (_, Mode::Plan) => {
            say("\nplan only — run `releasectl iap apply` to create the missing resources.".into())
        }
        (_, Mode::RetirePlan) => {
            say("\nplan only — run `releasectl iap retire` to take the products off sale.".into())
        }
        _ => {}
    }
    if json || args.json_out.is_some() || args.markdown_out.is_some() {
        let rep = RunReport {
            version: JSON_VERSION,
            app,
            command: match (kind, mode) {
                (Kind::Meta, Mode::Plan) => "meta-plan",
                (Kind::Meta, _) => "meta-apply",
                (_, Mode::Plan) => "plan",
                (_, Mode::Apply) => "apply",
                (_, Mode::RetirePlan) => "retire-plan",
                (_, Mode::Retire) => "retire",
            },
            summary: Summary::of(&all),
            actions: &all,
        };
        let json_text = serde_json::to_string_pretty(&rep).expect("serialize report");
        let md = releasectl_core::markdown::render(&rep);
        match args.output {
            Output::Markdown => print!("{md}"),
            Output::Json => println!("{json_text}"),
            Output::Text => {}
        }
        for (path, body) in [(&args.json_out, &json_text), (&args.markdown_out, &md)] {
            if let Some(path) = path
                && let Err(e) = std::fs::write(path, body)
            {
                eprintln!("write {}: {e}", path.display());
                had_fatal = true;
            }
        }
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
