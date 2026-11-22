use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{fs, thread};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("alphai-import-{label}-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(config: &Path, file: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alphai-tui"))
        .arg("--config")
        .arg(config)
        .arg("--import-positions")
        .arg(file)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn preview_apply_retry_and_broken_config() {
    let dir = Scratch::new("apply");
    let config = dir.0.join("portfolio.toml");
    let csv = dir.0.join("holdings.csv");
    let original = r#"watchlist = ["ZZZ"]
source = "no-such-source"
[future]
option = 12
[[positions]]
symbol = "abc"
qty = 1
avg_price = 20
note = "keep me"
[[positions]]
symbol = "OLD"
qty = 3
avg_price = 7
"#;
    fs::write(&csv, "Symbol,Qty,Average Cost\nABC,2,25\nXYZ,1,10").unwrap();
    let preview = success(run(&config, &csv, &["--dry-run"]));
    assert!(preview.contains("new      ABC"));
    assert!(!config.exists() && !config.with_extension("toml.lock").exists());
    fs::write(&config, original).unwrap();
    let preview = success(run(&config, &csv, &["--dry-run"]));
    assert!(preview.contains("changed  ABC") && preview.contains("was 1 @ 20"));
    assert!(preview.contains("kept     OLD") && preview.contains("new      XYZ"));
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
    assert!(success(run(&config, &csv, &[])).contains("Wrote 2 changes"));
    let saved = fs::read_to_string(&config).unwrap();
    let data: toml::Table = toml::from_str(&saved).unwrap();
    assert_eq!(
        data["watchlist"].as_array().unwrap()[0].as_str(),
        Some("ZZZ")
    );
    assert_eq!(data["future"]["option"].as_integer(), Some(12));
    let positions = data["positions"].as_array().unwrap();
    assert_eq!(positions.len(), 3);
    assert_eq!(positions[0]["note"].as_str(), Some("keep me"));
    assert_eq!(positions[0]["qty"].as_float(), Some(2.0));
    assert_eq!(positions[1]["symbol"].as_str(), Some("OLD"));
    let saved = format!(
        "{saved}\n# Keep this comment on an unchanged import.\n[[positions]]\nsymbol='BAD'\nqty=nan\navg_price=1\n"
    );
    fs::write(&config, &saved).unwrap();
    let retry = success(run(&config, &csv, &[]));
    assert!(retry.contains("Already up to date") && retry.contains("same     ABC"));
    assert_eq!(fs::read_to_string(&config).unwrap(), saved);
    for bad_csv in [
        "Symbol,Qty,Price\nABC,2,40",
        "Symbol,Qty,Average Cost\nABC,0,1",
        "Symbol,Quantity,CostBasisPrice,ListingExchange,CurrencyPrimary,AssetClass,LevelOfDetail\nASML,3,200,AEB,EUR,STK,SUMMARY\nASML,2,100",
    ] {
        fs::write(&csv, bad_csv).unwrap();
        assert!(!run(&config, &csv, &[]).status.success());
        assert_eq!(fs::read_to_string(&config).unwrap(), saved);
    }
    fs::write(&csv, "Symbol,Qty,Average Cost\nABC,2,25").unwrap();
    fs::write(&config, "watchlist = [").unwrap();
    for args in [&[][..], &["--dry-run"]] {
        assert!(!run(&config, &csv, args).status.success());
        assert_eq!(fs::read_to_string(&config).unwrap(), "watchlist = [");
    }
}

#[test]
fn concurrent_imports_preserve_each_others_holdings() {
    let dir = Scratch::new("parallel");
    let config = dir.0.join("config.toml");
    thread::scope(|scope| {
        for ticker in ["AAA", "BBB", "CCC", "DDD"] {
            let csv = dir.0.join(format!("{ticker}.csv"));
            fs::write(&csv, format!("Symbol,Shares,Avg Price\n{ticker},1,10")).unwrap();
            let config = &config;
            scope.spawn(move || success(run(config, &csv, &[])));
        }
    });
    let data: toml::Table = toml::from_str(&fs::read_to_string(config).unwrap()).unwrap();
    assert_eq!(data["positions"].as_array().unwrap().len(), 4);
}

#[test]
fn import_rejects_other_modes_and_positional_tickers() {
    let dir = Scratch::new("clap");
    let path = dir.0.join("missing");
    for args in [
        &["--json"][..],
        &["--once"],
        &["AAPL"],
        &["--events", "AAPL"],
        &["--earnings", "AAPL"],
    ] {
        assert_eq!(run(&path, &path, args).status.code(), Some(2));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_alphai-tui"))
        .arg("--dry-run")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
