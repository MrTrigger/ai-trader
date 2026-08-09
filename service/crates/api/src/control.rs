//! Running a control by invoking the bot, rather than by doing it here.
//!
//! # Why a subprocess and not a function call
//!
//! `flatten` and `adopt` move capital or rewrite our record of the book, and
//! this process holds no venue credentials — by design (spec §3.3), and not as
//! a formality: the process that serves a web page must not be one that can
//! place an order if it is ever wrong about who is asking.
//!
//! So the dashboard does not perform controls. It runs `bot`, which owns the
//! credential, the gates, and the run record. That is design principle 5 taken
//! literally — *every action the system can take is a CLI command you can run
//! yourself; interactive surfaces are lenses over that CLI, never a dependency
//! of it.* A button here and a shell there are the same code path, and there is
//! exactly one implementation of what "flatten" means.
//!
//! It also means the page cannot invent an action. The argument vector is built
//! from a fixed list below; nothing the browser sends becomes a flag, an
//! option, or a path. The only free text is the reason and the operator's name,
//! and both are passed as single argv entries to a process spawned without a
//! shell, so there is nothing for a quote to escape into.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the dashboard is allowed to ask for. Not a string that becomes a
/// subcommand — a closed set, matched exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Halt,
    Pause,
    Resume,
    Flatten,
    Adopt,
    AdoptAcceptingUnknownFills,
    /// Point the bot at a different venue. Not a control over the book, but the
    /// control that decides whose book everything else touches.
    ModePaper,
    ModeLiveReadonly,
    ModeLive,
}

impl Action {
    pub fn parse(route: &str) -> Option<Self> {
        match route {
            "/api/halt" => Some(Self::Halt),
            "/api/pause" => Some(Self::Pause),
            "/api/resume" => Some(Self::Resume),
            "/api/flatten" => Some(Self::Flatten),
            "/api/adopt" => Some(Self::Adopt),
            "/api/adopt-accepting-unknown-fills" => Some(Self::AdoptAcceptingUnknownFills),
            "/api/mode/paper" => Some(Self::ModePaper),
            "/api/mode/live-readonly" => Some(Self::ModeLiveReadonly),
            "/api/mode/live" => Some(Self::ModeLive),
            _ => None,
        }
    }

    fn subcommand(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Flatten => "flatten",
            Self::Adopt | Self::AdoptAcceptingUnknownFills => "adopt",
            Self::ModePaper | Self::ModeLiveReadonly | Self::ModeLive => "mode",
        }
    }

    /// Positional arguments, before the named ones. Only `mode set` has any,
    /// and the target comes from the matched action rather than the request, so
    /// the page cannot ask for a mode that is not on this list.
    fn positional(self) -> &'static [&'static str] {
        match self {
            Self::ModePaper => &["set", "paper"],
            Self::ModeLiveReadonly => &["set", "live-readonly"],
            Self::ModeLive => &["set", "live"],
            _ => &[],
        }
    }

    /// Extra arguments the subcommand needs. Fixed per action, never supplied
    /// by the caller.
    fn extra(self) -> &'static [&'static str] {
        match self {
            Self::Flatten | Self::Adopt => &["--confirm"],
            Self::AdoptAcceptingUnknownFills => &["--confirm", "--accept-unknown-fills"],
            Self::ModePaper | Self::ModeLiveReadonly | Self::ModeLive => &["--confirm"],
            _ => &[],
        }
    }

    /// Whether this action can move capital or grant trading authority.
    ///
    /// Not used to forbid anything — the operator asked for these controls — but
    /// the page marks them differently and the log says which kind it was.
    pub fn is_consequential(self) -> bool {
        !matches!(self, Self::Halt | Self::Pause | Self::ModePaper)
    }

    /// Whether choosing this puts real capital within reach.
    pub fn goes_live(self) -> bool {
        matches!(self, Self::ModeLive)
    }
}

/// Which bot this page is about, and whether it may be driven.
///
/// Two separate questions that used to be one. The config says which bot to
/// READ - its id, its venue, its mode - and the binary is what makes the write
/// controls available. A deployment can legitimately want the first without
/// the second: the cluster dashboard is a lens whose hands live in `kubectl
/// exec`, where an intervention is authenticated and audited. Conflating them
/// meant removing the binary also blinded the page, which then read
/// file-backed state and reported a running bot as halted.
#[derive(Debug, Clone)]
pub struct BotCommand {
    /// `None` when the page may look but not touch.
    pub binary: Option<PathBuf>,
    pub config: PathBuf,
}

impl BotCommand {
    /// A binary still requires a config. The reverse is now allowed and means
    /// read-only: a control that fails at the moment someone needs it is worse
    /// than one that was never offered, but a page that cannot say what the
    /// bot is doing is worse than both.
    pub fn new(binary: Option<String>, config: Option<String>) -> Result<Option<Self>, String> {
        match (binary, config) {
            (None, None) => Ok(None),
            (binary, Some(c)) => {
                let config = PathBuf::from(c);
                if !config.is_file() {
                    return Err(format!("--bot-config {} is not a file", config.display()));
                }
                let binary = match binary {
                    Some(b) => {
                        let b = PathBuf::from(b);
                        if !b.is_file() {
                            return Err(format!("--bot {} is not a file", b.display()));
                        }
                        Some(b)
                    }
                    None => None,
                };
                Ok(Some(Self { binary, config }))
            }
            (Some(_), None) => Err(
                "--bot needs --bot-config: a binary with no config would offer controls that \
                 fail at the moment someone needs them"
                    .into(),
            ),
        }
    }

    /// True when the write controls may be offered at all.
    pub fn can_drive(&self) -> bool {
        self.binary.is_some()
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run one control.
///
/// The whole argument vector is assembled here. `reason` and `by` are the only
/// values that come from the request, and each is one argv entry to a process
/// spawned without a shell — no quoting, no globbing, nothing to escape.
pub fn run(bot: &BotCommand, action: Action, reason: &str, by: &str) -> Result<Outcome, String> {
    // Refused here as well as at the routing layer: a read-only deployment
    // must not be one edit away from becoming a writable one.
    let Some(binary) = bot.binary.as_ref() else {
        return Err(
            "this dashboard is read-only: it was started without --bot. Interventions go \
             through the bot CLI, where they are authenticated and audited."
                .into(),
        );
    };
    let mut cmd = Command::new(binary);
    cmd.arg("--config")
        .arg(&bot.config)
        .arg(action.subcommand());
    for a in action.positional() {
        cmd.arg(a);
    }
    cmd.arg("--reason").arg(reason).arg("--by").arg(by);
    for a in action.extra() {
        cmd.arg(a);
    }
    // Nothing inherited. A subprocess that can see this process's environment
    // is a subprocess that can be steered by it.
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", binary.display()))?;
    Ok(Outcome {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// The command a human would type, for the page to show.
pub fn as_typed(bot: Option<&BotCommand>, action: Action) -> String {
    let config = bot
        .map(|b| b.config.display().to_string())
        .unwrap_or_else(|| "<file>".into());
    let binary = bot
        .and_then(|b| b.binary.as_ref())
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .unwrap_or_else(|| "bot".into());
    let mut s = format!("{binary} --config {config} {}", action.subcommand());
    for a in action.positional() {
        s.push(' ');
        s.push_str(a);
    }
    s.push_str(" --reason \"...\" --by you");
    for a in action.extra() {
        s.push(' ');
        s.push_str(a);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_routes_become_actions() {
        assert_eq!(Action::parse("/api/halt"), Some(Action::Halt));
        assert_eq!(Action::parse("/api/flatten"), Some(Action::Flatten));
        // The failure this prevents: a route fragment becoming a subcommand.
        assert_eq!(Action::parse("/api/rm"), None);
        assert_eq!(Action::parse("/api/halt;rm -rf /"), None);
        assert_eq!(Action::parse("/api/../../etc"), None);
    }

    #[test]
    fn the_argument_vector_never_takes_a_flag_from_the_caller() {
        // Whatever the browser sends is a value, never an option. Assembling
        // the vector from a closed set is what makes that true; this asserts
        // the set has not grown a hole.
        for a in [
            Action::Halt,
            Action::Pause,
            Action::Resume,
            Action::Flatten,
            Action::Adopt,
            Action::AdoptAcceptingUnknownFills,
        ] {
            assert!(!a.subcommand().starts_with('-'));
            assert!(a.extra().iter().all(|x| x.starts_with("--")));
        }
    }

    #[test]
    fn adopt_only_accepts_unknown_fills_when_that_action_was_chosen() {
        // Two separate actions rather than a boolean off the request body: a
        // flag that arrives as data is a flag that can arrive by accident.
        assert!(!Action::Adopt.extra().contains(&"--accept-unknown-fills"));
        assert!(Action::AdoptAcceptingUnknownFills
            .extra()
            .contains(&"--accept-unknown-fills"));
    }

    #[test]
    fn stopping_is_not_consequential_and_everything_else_is() {
        assert!(!Action::Halt.is_consequential());
        assert!(!Action::Pause.is_consequential());
        assert!(Action::Resume.is_consequential());
        assert!(Action::Flatten.is_consequential());
        assert!(Action::Adopt.is_consequential());
    }

    #[test]
    fn a_bot_binary_without_a_config_is_refused_at_startup() {
        let err = BotCommand::new(Some("/bin/true".into()), None).unwrap_err();
        assert!(err.contains("needs --bot-config"), "{err}");
        assert!(BotCommand::new(None, None).unwrap().is_none());
    }

    /// A config without a binary is a lens: it names the bot to read and
    /// offers no way to drive it. Both halves are asserted, because the
    /// dangerous half-failure is a page that looks read-only and is not.
    #[test]
    fn a_config_without_a_binary_reads_but_cannot_drive() {
        let cfg = std::env::temp_dir().join("api-lens-test.json");
        std::fs::write(&cfg, "{}").unwrap();
        let bot = BotCommand::new(None, Some(cfg.display().to_string()))
            .unwrap()
            .expect("a config alone is a valid read-only lens");
        assert!(!bot.can_drive());
        let err = run(&bot, Action::Halt, "why", "who").unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        std::fs::remove_file(&cfg).ok();
    }
}
