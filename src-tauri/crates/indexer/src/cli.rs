//! `ferrum-indexer.exe` command line (FER-474). Strict: unknown or repeated
//! arguments are refused before anything is opened.
//!
//! ```text
//! ferrum-indexer.exe                          started by the SCM (service)
//! ferrum-indexer.exe install [--mode M | --mode-if-new M]     (admin)
//! ferrum-indexer.exe uninstall [--purge-data]                 (admin)
//! ferrum-indexer.exe stop                                     (admin)
//! ferrum-indexer.exe has-config        exit 0 = valid config exists (admin)
//! ferrum-indexer.exe status [--pipe S]
//! ferrum-indexer.exe set-mode M [--pipe S]
//! ferrum-indexer.exe index-status [--pipe S]
//! ferrum-indexer.exe --console --pipe S [--data-dir DIR] [--mode M] [--index]
//! ```
//! `M` is `high_performance` or `shared`; `S` is a dev pipe suffix.

use crate::proto::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Service,
    Install {
        mode: InstallMode,
    },
    Uninstall {
        purge_data: bool,
    },
    Stop,
    HasConfig,
    Status {
        pipe: Option<String>,
    },
    SetMode {
        mode: Mode,
        pipe: Option<String>,
    },
    IndexStatus {
        pipe: Option<String>,
    },
    Console {
        pipe: String,
        data_dir: Option<String>,
        mode: Option<Mode>,
        /// Run the index worker too (FER-475). Without admin rights every
        /// volume reports `access_denied`; elevated, it really indexes.
        index: bool,
    },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    Set(Mode),
    IfNew(Mode),
    Keep,
}

pub const USAGE: &str = "usage:
  ferrum-indexer install [--mode high_performance|shared | --mode-if-new high_performance|shared]
  ferrum-indexer uninstall [--purge-data]
  ferrum-indexer stop
  ferrum-indexer has-config
  ferrum-indexer status [--pipe SUFFIX]
  ferrum-indexer set-mode high_performance|shared [--pipe SUFFIX]
  ferrum-indexer index-status [--pipe SUFFIX]
  ferrum-indexer --console --pipe SUFFIX [--data-dir DIR] [--mode high_performance|shared] [--index]";

fn mode_arg(v: Option<&String>) -> Result<Mode, String> {
    let v = v.ok_or("missing mode")?;
    Mode::parse(v).ok_or_else(|| format!("invalid mode {v:?}"))
}

/// Parse argv WITHOUT the program name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Service);
    };
    let rest = &args[1..];
    // Flag/value pairs after the subcommand; each flag at most once.
    let pairs =
        |allowed: &[&str], switches: &[&str]| -> Result<Vec<(String, Option<String>)>, String> {
            let mut out: Vec<(String, Option<String>)> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                let a = &rest[i];
                if out.iter().any(|(k, _)| k == a) {
                    return Err(format!("{a} given twice"));
                }
                if switches.contains(&a.as_str()) {
                    out.push((a.clone(), None));
                    i += 1;
                } else if allowed.contains(&a.as_str()) {
                    let v = rest.get(i + 1).ok_or(format!("{a} needs a value"))?;
                    out.push((a.clone(), Some(v.clone())));
                    i += 2;
                } else {
                    return Err(format!("unexpected argument {a:?}"));
                }
            }
            Ok(out)
        };
    let get = |p: &[(String, Option<String>)], k: &str| -> Option<String> {
        p.iter().find(|(x, _)| x == k).and_then(|(_, v)| v.clone())
    };
    let pipe_ok = |s: Option<String>| -> Result<Option<String>, String> {
        match s {
            Some(s) if crate::pipe_suffix_ok(&s) => Ok(Some(s)),
            Some(s) => Err(format!("invalid pipe suffix {s:?}")),
            None => Ok(None),
        }
    };
    match first.as_str() {
        "install" => {
            let p = pairs(&["--mode", "--mode-if-new"], &[])?;
            let mode = match (get(&p, "--mode"), get(&p, "--mode-if-new")) {
                (Some(_), Some(_)) => return Err("--mode and --mode-if-new are exclusive".into()),
                (Some(m), None) => InstallMode::Set(mode_arg(Some(&m))?),
                (None, Some(m)) => InstallMode::IfNew(mode_arg(Some(&m))?),
                (None, None) => InstallMode::Keep,
            };
            Ok(Command::Install { mode })
        }
        "uninstall" => {
            let p = pairs(&[], &["--purge-data"])?;
            Ok(Command::Uninstall {
                purge_data: !p.is_empty(),
            })
        }
        "stop" => pairs(&[], &[]).map(|_| Command::Stop),
        "has-config" => pairs(&[], &[]).map(|_| Command::HasConfig),
        "status" => {
            let p = pairs(&["--pipe"], &[])?;
            Ok(Command::Status {
                pipe: pipe_ok(get(&p, "--pipe"))?,
            })
        }
        "set-mode" => {
            let mode = mode_arg(rest.first())?;
            let tail: Vec<String> = rest[1..].to_vec();
            let sub = parse(&[vec!["status".to_string()], tail].concat())?;
            let Command::Status { pipe } = sub else {
                unreachable!()
            };
            Ok(Command::SetMode { mode, pipe })
        }
        "index-status" => {
            let p = pairs(&["--pipe"], &[])?;
            Ok(Command::IndexStatus {
                pipe: pipe_ok(get(&p, "--pipe"))?,
            })
        }
        "--console" => {
            let p = pairs(&["--pipe", "--data-dir", "--mode"], &["--index"])?;
            let pipe = pipe_ok(get(&p, "--pipe"))?.ok_or("--console needs --pipe SUFFIX")?;
            let mode = get(&p, "--mode").map(|m| mode_arg(Some(&m))).transpose()?;
            Ok(Command::Console {
                pipe,
                data_dir: get(&p, "--data-dir"),
                mode,
                index: p.iter().any(|(k, _)| k == "--index"),
            })
        }
        "help" | "--help" | "-h" | "/?" => Ok(Command::Help),
        other => Err(format!("unknown command {other:?}")),
    }
}

#[cfg(windows)]
pub use win::main;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::client::{Client, ClientError};
    use crate::pipe::Endpoint;
    use crate::proto::ErrorCode;
    use crate::server::{self, ServerState};
    use crate::service::{self, ModeChoice};
    use crate::store;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn endpoint(pipe: &Option<String>) -> Endpoint {
        match pipe {
            Some(s) => Endpoint::Dev(s.clone()),
            None => Endpoint::Service,
        }
    }

    fn connect(pipe: &Option<String>) -> Result<Client, ClientError> {
        Client::connect(&endpoint(pipe), Duration::from_secs(5))
    }

    /// Process entry. Returns the exit code: 0 ok, 1 failure, 2 usage,
    /// 3 = needs elevation (set-mode), 4 = service not running.
    pub fn main(version: &str) -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let cmd = match parse(&args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}\n{USAGE}");
                return 2;
            }
        };
        let done = |r: Result<(), String>| match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
        match cmd {
            Command::Help => {
                println!("{USAGE}");
                0
            }
            Command::Service => match service::run_dispatcher(version) {
                Ok(true) => 0,
                Ok(false) => {
                    eprintln!(
                        "ferrum-indexer is a Windows service (FerrumIndexer); run it from the SCM.\n{USAGE}"
                    );
                    2
                }
                Err(e) => done(Err(e)),
            },
            Command::Install { mode } => done(service::install(match mode {
                InstallMode::Set(m) => ModeChoice::Set(m),
                InstallMode::IfNew(m) => ModeChoice::IfNew(m),
                InstallMode::Keep => ModeChoice::Keep,
            })),
            Command::Uninstall { purge_data } => done(service::uninstall(purge_data)),
            Command::Stop => done(service::stop()),
            Command::HasConfig => {
                if service::has_config() {
                    0
                } else {
                    1
                }
            }
            Command::Status { pipe } => {
                let r = connect(&pipe).and_then(|mut c| {
                    let s = c.status()?;
                    println!(
                        "state={} mode={} index={} version={} uptime={}s console={} can_lower_privacy={} degraded={}",
                        s.state.as_str(),
                        s.mode.as_str(),
                        s.index.as_str(),
                        s.version,
                        s.uptime_secs,
                        s.console,
                        c.hello.can_lower_privacy,
                        s.degraded
                    );
                    Ok(())
                });
                client_exit(r)
            }
            Command::IndexStatus { pipe } => {
                let r = connect(&pipe).and_then(|mut c| {
                    let s = c.index_status()?;
                    println!(
                        "paused={} memory={}KiB limit={}KiB volumes={}",
                        s.paused,
                        s.memory_bytes / 1024,
                        s.memory_limit / 1024,
                        s.volumes.len()
                    );
                    for v in &s.volumes {
                        println!(
                            "{}: state={} error={} entries={} memory={}KiB live={} journal={:#018x} next_usn={} progress={}",
                            v.letter as char,
                            v.state.as_str(),
                            v.error.as_str(),
                            v.entries,
                            v.memory_bytes / 1024,
                            v.live,
                            v.journal_id,
                            v.next_usn,
                            v.progress_permille
                        );
                    }
                    Ok(())
                });
                client_exit(r)
            }
            Command::SetMode { mode, pipe } => {
                let r = connect(&pipe).and_then(|mut c| {
                    let changed = c.set_mode(mode)?;
                    println!(
                        "mode={} ({})",
                        mode.as_str(),
                        if changed { "changed" } else { "unchanged" }
                    );
                    Ok(())
                });
                client_exit(r)
            }
            Command::Console {
                pipe,
                data_dir,
                mode,
                index,
            } => done(console(&pipe, data_dir, mode, index, version)),
        }
    }

    fn client_exit(r: Result<(), ClientError>) -> i32 {
        match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                match e {
                    ClientError::Refused {
                        code: ErrorCode::NeedsElevation,
                        ..
                    } => 3,
                    ClientError::NotRunning => 4,
                    _ => 1,
                }
            }
        }
    }

    /// Console server for development and tests: runs as the CURRENT user
    /// on a dev pipe that only this user may open; the config lives in a
    /// plain folder (default `%TEMP%\ferrum-indexer-dev-<suffix>`). Ctrl+C
    /// ends the process.
    fn console(
        suffix: &str,
        data_dir: Option<String>,
        mode: Option<Mode>,
        index: bool,
        version: &str,
    ) -> Result<(), String> {
        store::log_to_stderr();
        let ep = Endpoint::dev(suffix).ok_or("invalid pipe suffix")?;
        let dir = data_dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join(format!("ferrum-indexer-dev-{suffix}")));
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("data dir: {e}"))?;
        }
        let trust = store::Trust::dev_current_user().map_err(|e| e.to_string())?;
        let (dd, _) = store::DataDir::prepare(&dir, trust).map_err(|e| format!("data dir: {e}"))?;
        let stored = dd
            .read_config()
            .ok()
            .flatten()
            .filter(|(_, e)| e.is_none())
            .map(|(c, _)| c.mode);
        let mode = mode.or(stored).unwrap_or(Mode::Shared);
        let dd = Arc::new(dd);
        // Console (dev pipe: only the current user can connect, never the
        // service): queries on, so the query surface can be exercised.
        let mut state = ServerState::new(version, true, mode, Some(dd.clone())).with_queries(true);
        if index {
            let shared = crate::engine::IndexShared::new(mode);
            crate::volume::start_worker(Some(&dd), shared.clone())
                .map_err(|e| format!("index worker: {e}"))?;
            state = state.with_index(shared);
        }
        let state = Arc::new(state);
        store::log(&format!(
            "console server v{version}, mode {}, data {}",
            mode.as_str(),
            dir.display()
        ));
        server::run(&ep, state, Arc::new(AtomicBool::new(false))).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Command, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn valid_commands() {
        assert_eq!(p(&[]), Ok(Command::Service));
        assert_eq!(
            p(&["install"]),
            Ok(Command::Install {
                mode: InstallMode::Keep
            })
        );
        assert_eq!(
            p(&["install", "--mode", "high_performance"]),
            Ok(Command::Install {
                mode: InstallMode::Set(Mode::HighPerformance)
            })
        );
        assert_eq!(
            p(&["install", "--mode-if-new", "shared"]),
            Ok(Command::Install {
                mode: InstallMode::IfNew(Mode::Shared)
            })
        );
        assert_eq!(
            p(&["uninstall", "--purge-data"]),
            Ok(Command::Uninstall { purge_data: true })
        );
        assert_eq!(
            p(&["uninstall"]),
            Ok(Command::Uninstall { purge_data: false })
        );
        assert_eq!(p(&["stop"]), Ok(Command::Stop));
        assert_eq!(p(&["has-config"]), Ok(Command::HasConfig));
        assert_eq!(p(&["status"]), Ok(Command::Status { pipe: None }));
        assert_eq!(
            p(&["set-mode", "shared", "--pipe", "t1"]),
            Ok(Command::SetMode {
                mode: Mode::Shared,
                pipe: Some("t1".into())
            })
        );
        assert_eq!(
            p(&["--console", "--pipe", "dev1", "--mode", "high_performance"]),
            Ok(Command::Console {
                pipe: "dev1".into(),
                data_dir: None,
                mode: Some(Mode::HighPerformance),
                index: false,
            })
        );
        assert_eq!(
            p(&["--console", "--pipe", "dev1", "--index"]),
            Ok(Command::Console {
                pipe: "dev1".into(),
                data_dir: None,
                mode: None,
                index: true,
            })
        );
        assert_eq!(
            p(&["index-status", "--pipe", "t2"]),
            Ok(Command::IndexStatus {
                pipe: Some("t2".into())
            })
        );
    }

    #[test]
    fn invalid_commands() {
        for a in [
            &["bogus"][..],
            &["install", "--mode"],
            &["install", "--mode", "fast"],
            &["install", "--mode", "shared", "--mode-if-new", "shared"],
            &["install", "--mode", "shared", "--mode", "shared"],
            &["uninstall", "--purge-data", "--purge-data"],
            &["uninstall", "--force"],
            &["stop", "now"],
            &["set-mode"],
            &["set-mode", "Shared"],
            &["set-mode", "shared", "--pipe", "../x"],
            &["status", "--pipe", "a b"],
            &["--console"],
            &["--console", "--pipe", ""],
            &["--console", "--pipe", "x", "--extra", "1"],
            &["--console", "--pipe", "x", "--index", "--index"],
            &["index-status", "now"],
            &["index-status", "--pipe", "a/b"],
        ] {
            assert!(p(a).is_err(), "{a:?}");
        }
    }
}
