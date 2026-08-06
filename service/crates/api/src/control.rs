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

/// Where the bot lives. Absent means the write controls are simply unavailable.
#[derive(Debug, Clone)]
pub struct BotCommand {
    pub binary: PathBuf,
    pub config: PathBuf,
}

impl BotCommand {
    /// Both or neither. A binary with no config, or a config with no binary,
    /// would fail at the first click rather than at startup — and a control
    /// that fails only when someone urgently needs it is worse than one that
    /// was never offered.
    pub fn new(binary: Option<String>, config: Option<String>) -> Result<Option<Self>, String> {
        match (binary, config) {
            (None, None) => Ok(None),
            (Some(b), Some(c)) => {
                let binary = PathBuf::from(b);
                let config = PathBuf::from(c);
                if !binary.is_file() {
                    return Err(format!("--bot {} is not a file", binary.display()));
                }
                if !config.is_file() {
                    return Err(format!("--bot-config {} is not a file", config.display()));
                }
                Ok(Some(Self { binary, config }))
            }
            _ => Err(
                "--bot and --bot-config go together: one without the other would offer controls \
                 that fail at the moment someone needs them"
                    .into(),
            ),
        }
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
    let mut cmd = Command::new(&bot.binary);
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
        .map_err(|e| format!("cannot run {}: {e}", bot.binary.display()))?;
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
        .map(|b| {
            Path::new(&b.binary)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| b.binary.display().to_string())
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
        assert!(err.contains("go together"));
        assert!(BotCommand::new(None, None).unwrap().is_none());
    }
}
