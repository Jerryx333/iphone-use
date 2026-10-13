use clap::Parser;
use serde_json::json;
use std::time::{Duration, Instant};
#[derive(Debug, Parser)]
#[command(
    about = "USB/RPPairing IDE-side probe and XCTest protocol trace",
    subcommand_required = true
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Launch the installed formal runner and record all DTX traffic (no HTTP)
    XctestTrace {
        #[arg(long, value_parser = nonempty)]
        udid: String,
        #[arg(long, value_parser = nonempty)]
        bundle: Option<String>,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        observe_secs: u64,
        /// New JSONL file (refuses overwrite); defaults to stdout
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Run the original USB DDI/session/observation probe
    Probe {
        #[arg(long, value_parser = nonempty)]
        udid: String,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        authorize_pid: Option<u32>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
        observe_secs: u64,
    },
    /// Create a NEW RemotePairing record over USB (unlock phone and tap Trust)
    RpPair {
        #[arg(long, value_parser = nonempty)]
        udid: String,
        #[arg(long, value_parser = nonempty)]
        name: String,
        /// New output file; refuses overwrite, created with mode 0600
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Verify an existing RP record and probe DDI + IDE sessions at IP:port
    RpConnect {
        #[arg(long)]
        pairing: std::path::PathBuf,
        #[arg(long)]
        addr: std::net::SocketAddr,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
        observe_secs: u64,
    },
}
fn nonempty(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("value cannot be empty".into())
    } else {
        Ok(s.into())
    }
}
fn output(value: serde_json::Value) {
    println!("{value}");
}
#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(e) = run(args).await {
        output(json!({"step":"error","error":e.to_string(),"detail":format!("{e:?}")}));
        std::process::exit(1);
    }
}
async fn run(args: Args) -> self_boot::Result<()> {
    let start = Instant::now();
    let (mut session, authorize_pid, observe_secs, step) = match args.command {
        Command::XctestTrace {
            udid,
            bundle,
            observe_secs,
            out,
        } => {
            return self_boot::xctest_trace::run(
                &udid,
                bundle.as_deref(),
                Duration::from_secs(observe_secs),
                out.as_deref(),
            )
            .await;
        }
        Command::RpPair { udid, name, out } => return pairing::pair(&udid, &name, &out).await,
        Command::Probe {
            udid,
            authorize_pid,
            observe_secs,
        } => (
            self_boot::connect_usb(&udid).await?,
            authorize_pid,
            observe_secs,
            "connect_usb",
        ),
        Command::RpConnect {
            pairing,
            addr,
            observe_secs,
        } => (
            self_boot::connect_rp(pairing, addr, None).await?,
            None,
            observe_secs,
            "connect_rp",
        ),
    };
    output(
        json!({"step":step,"elapsed_ms":start.elapsed().as_millis(),"services":session.rsd.services.len(),"state":session.state()}),
    );
    let mut events = session.subscribe();
    let result: self_boot::Result<()> = async {
        let start = Instant::now(); let status = session.ddi_status().await?;
        output(json!({"step":"ddi_status","elapsed_ms":start.elapsed().as_millis(),"result":status}));
        let id = uuid::Uuid::new_v4(); let start = Instant::now();
        let tests = session.start_test_sessions(id).await?;
        output(json!({"step":"start_test_sessions","elapsed_ms":start.elapsed().as_millis(),"uuid":id.to_string(),"control_reply":tests.control_reply,"test_reply":tests.test_reply}));
        if let Some(pid) = authorize_pid {
            let start = Instant::now(); let reply = tests.authorize_pid(pid).await?;
            output(json!({"step":"authorize_pid","elapsed_ms":start.elapsed().as_millis(),"pid":pid,"reply":reply}));
        }
        let observed_at = Instant::now();
        let until = tokio::time::sleep(Duration::from_secs(observe_secs)); tokio::pin!(until);
        loop { tokio::select! {
            _ = &mut until => break,
            event = events.recv() => match event {
                Ok(event) => output(json!({"step":"message","event":event})),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => output(json!({"step":"lagged","count":n})),
                Err(_) => break,
            }
        } }
        output(json!({"step":"observe","elapsed_ms":observed_at.elapsed().as_millis(),"state":session.state()}));
        drop(tests); Ok(())
    }.await;
    let start = Instant::now();
    let closed = session.close().await;
    output(
        json!({"step":"disconnect","elapsed_ms":start.elapsed().as_millis(),"state":session.state()}),
    );
    result?;
    closed
}
#[path = "../probe_pairing.rs"]
mod pairing;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_validation() {
        for argv in [
            vec!["probe"],
            vec!["probe", "xctest-trace", "--udid", " "],
            vec![
                "probe",
                "xctest-trace",
                "--udid",
                "abc",
                "--observe-secs",
                "0",
            ],
            vec!["probe", "probe", "--udid", " "],
            vec!["probe", "probe", "--udid", "abc", "--authorize-pid", "0"],
            vec!["probe", "rp-connect", "--pairing", "x", "--addr", "bad"],
            vec![
                "probe",
                "rp-connect",
                "--pairing",
                "x",
                "--addr",
                "10.7.0.1:49152",
                "--observe-secs",
                "0",
            ],
            vec!["probe", "rp-pair", "--udid", "abc", "--name", "x"],
        ] {
            assert!(Args::try_parse_from(argv).is_err());
        }
        assert!(Args::try_parse_from(["probe", "probe", "--udid", "abc"]).is_ok());
        let args = Args::try_parse_from(["probe", "xctest-trace", "--udid", "abc"]).unwrap();
        assert!(matches!(
            args.command,
            Command::XctestTrace {
                observe_secs: 30,
                ..
            }
        ));
        assert!(
            Args::try_parse_from([
                "probe", "rp-pair", "--udid", "abc", "--name", "host", "--out", "x"
            ])
            .is_ok()
        );
        assert!(
            Args::try_parse_from([
                "probe",
                "rp-connect",
                "--pairing",
                "x",
                "--addr",
                "10.7.0.1:49152"
            ])
            .is_ok()
        );
    }
}
