//! The provider identities: the kinds the engine runs, the names config files spell, and
//! how each kind's interactive CLI is launched.

use std::ffi::OsStr;

use schemars::JsonSchema;

use patok_core::settings::{EnumSpelling, config_enum, config_enum_aliased};
use patok_core::utils::executable::cli_in;

config_enum! {
    /// The providers the engine can run today.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum ProviderKind {
        #[default]
        Claude => "claude",
        Codex => "codex",
        Vibe => "vibe",
    }
}

impl ProviderKind {
    /// The capitalized spelling, as the output frame title shows it.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            // T2.4 switches the TUI to the catalog harness name.
            Self::Vibe => "Mistral",
        }
    }

    /// How to start this provider's own interactive CLI in an embedded terminal.
    pub fn interactive_launch(self) -> InteractiveLaunch {
        match self {
            Self::Claude => InteractiveLaunch {
                program: Self::Claude.info().binary,
                args: &[],
                // Marks a nested Claude session; patok may itself run inside Claude Code,
                // which would otherwise make the child refuse to start.
                env: &[("CLAUDECODE", "")],
            },
            Self::Codex => InteractiveLaunch {
                program: Self::Codex.info().binary,
                args: &[],
                env: &[],
            },
            Self::Vibe => InteractiveLaunch {
                program: Self::Vibe.info().binary,
                args: &[],
                env: &[],
            },
        }
    }

    /// Whether this kind's interactive program is an executable in `path`. The picker
    /// passes `std::env::var_os("PATH")` and shows the kind disabled when this is false.
    pub fn interactive_available_in(self, path: Option<&OsStr>) -> bool {
        cli_in(self.interactive_launch().program, path)
    }
}

/// How to launch a provider's own interactive CLI in an embedded terminal: no prompt,
/// the user drives the session. The TUI builds the PTY command from this table.
///
/// Build the child env in this order: remove every host variable for which
/// [`strip_from_interactive_env`] is true, set [`INTERACTIVE_TERMINAL_ENV`], then set `env`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractiveLaunch {
    /// The executable looked up on `PATH`.
    pub program: &'static str,
    /// Arguments; empty for a plain prompt-less start.
    pub args: &'static [&'static str],
    /// Provider-specific variables, set after [`INTERACTIVE_TERMINAL_ENV`].
    pub env: &'static [(&'static str, &'static str)],
}

/// Terminal variables set on every interactive child: the vt100 emulator understands
/// `xterm-256color` and truecolor.
pub const INTERACTIVE_TERMINAL_ENV: [(&str, &str); 2] =
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// Host variables dropped by exact name.
const STRIPPED_ENV_NAMES: &[&str] = &[
    "NO_COLOR",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERMINFO",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "VTE_VERSION",
    "WT_SESSION",
    "COLORFGBG",
];

/// Host variables dropped by name prefix.
const STRIPPED_ENV_PREFIXES: &[&str] = &[
    "ZELLIJ",
    "KITTY_",
    "WEZTERM_",
    "GHOSTTY_",
    "ALACRITTY_",
    "KONSOLE_",
    "ITERM_",
];

/// Whether a host variable is dropped from an interactive child's env: `NO_COLOR` would
/// suppress the color the emulator renders, and host-terminal/multiplexer variables would
/// make the child talk to the outer terminal (kitty/wezterm/tmux protocols) instead of
/// the embedded vt100 one.
pub fn strip_from_interactive_env(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        STRIPPED_ENV_NAMES.contains(&name)
            || STRIPPED_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
    })
}

config_enum_aliased! {
    /// A provider name as written in config files:
    /// trimmed, case-insensitive, aliases accepted.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum ProviderName {
        #[default]
        Claude => "claude",
        Codex => "codex",
        Vibe => "vibe", [alias "mistral"],
    }
}

impl JsonSchema for ProviderName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ProviderName".into()
    }

    /// Every accepted spelling, aliases included, from the list the parser reads.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A provider name as written in config files:\ntrimmed, case-insensitive, aliases accepted.",
            "type": "string",
            "enum": Self::accepted().collect::<Vec<_>>(),
        })
    }
}

impl ProviderName {
    /// The engine-implemented kind of this provider name.
    pub fn kind(self) -> Option<ProviderKind> {
        match self {
            Self::Claude => Some(ProviderKind::Claude),
            Self::Codex => Some(ProviderKind::Codex),
            Self::Vibe => Some(ProviderKind::Vibe),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_programs_are_the_provider_binaries() {
        assert_eq!(ProviderKind::Claude.interactive_launch().program, "claude");
        assert_eq!(ProviderKind::Codex.interactive_launch().program, "codex");
        assert_eq!(ProviderKind::Vibe.interactive_launch().program, "vibe");
    }

    #[test]
    fn interactive_launches_are_prompt_less() {
        for (kind, _) in <ProviderKind as EnumSpelling>::VARIANTS {
            assert!(kind.interactive_launch().args.is_empty());
        }
    }

    #[test]
    fn only_claude_clears_the_nested_session_marker() {
        assert_eq!(
            ProviderKind::Claude.interactive_launch().env,
            &[("CLAUDECODE", "")]
        );
        assert!(ProviderKind::Codex.interactive_launch().env.is_empty());
        assert!(ProviderKind::Vibe.interactive_launch().env.is_empty());
    }

    #[test]
    fn interactive_terminal_env_is_xterm_256color_truecolor() {
        assert_eq!(
            INTERACTIVE_TERMINAL_ENV,
            [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
        );
    }

    #[test]
    fn strip_from_interactive_env_drops_no_color_and_host_terminal_vars() {
        for name in [
            "NO_COLOR",
            "TMUX",
            "KITTY_WINDOW_ID",
            "WEZTERM_PANE",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "TERM_PROGRAM",
        ] {
            assert!(strip_from_interactive_env(OsStr::new(name)), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "TERM",
            "COLORTERM",
            "LANG",
            "no_color",
            "MY_TMUX",
        ] {
            assert!(!strip_from_interactive_env(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn interactive_availability_is_a_path_lookup() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("vibe", 0o755), ("codex", 0o644)] {
            let file = dir.path().join(name);
            std::fs::write(&file, "").unwrap();
            std::fs::set_permissions(&file, PermissionsExt::from_mode(mode)).unwrap();
        }
        let path = std::env::join_paths([dir.path()]).unwrap();
        let path = Some(path.as_os_str());
        assert!(ProviderKind::Vibe.interactive_available_in(path));
        assert!(!ProviderKind::Claude.interactive_available_in(path));
        assert!(!ProviderKind::Codex.interactive_available_in(path));
        for (kind, _) in <ProviderKind as EnumSpelling>::VARIANTS {
            assert!(!kind.interactive_available_in(None));
        }
    }

    /// The display names the output frame title shows, one per implemented kind.
    #[test]
    fn display_names_capitalize_the_spellings() {
        // T2.4 removes `display_name`; until then Vibe still shows the provider name.
        for (kind, expected) in [
            (ProviderKind::Claude, "Claude"),
            (ProviderKind::Codex, "Codex"),
            (ProviderKind::Vibe, "Mistral"),
        ] {
            let display = kind.display_name();
            assert!(display.starts_with(|c: char| c.is_uppercase()));
            assert_eq!(display, expected);
        }
    }
}
