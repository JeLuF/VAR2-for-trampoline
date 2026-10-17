//! Keyboard shortcuts for all review and marker functions (INP-001).
//!
//! [`SHORTCUTS`] is the only definition: [`read`] turns key presses into actions from
//! it, and the help overlay (INP-002) shows it, so the two can't disagree.

use eframe::egui::{self, Key, Modifiers};

/// A user action, from a shortcut, a button or the remote control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    /// Play backwards / pause (REV-005).
    PlayReverse,
    /// Pause, whatever is playing (remote control).
    Pause,
    /// One frame forward (+1) or backward (−1) (REV-004).
    Step(i32),
    /// Jump by seconds (REV-006).
    Jump(i64),
    Faster,
    Slower,
    /// Set the speed, in thousandths (remote control): 2000 = 2×, 100 = 0.1×.
    Speed(u32),
    Review,
    Live,
    SetMarker,
    PreviousMarker,
    NextMarker,
    /// Toggle camera slot full screen (VIEW-002).
    FullScreen(usize),
    /// Back to the grid; closes the help first.
    ExitFullScreen,
    /// Show / hide the keyboard shortcuts (INP-002).
    Help,
}

/// One line of the help: keys and description, and the key bindings behind it.
pub struct Shortcut {
    pub keys: &'static str,
    pub description: &'static str,
    pub bindings: &'static [(Modifiers, Key, Action)],
}

const NONE: Modifiers = Modifiers::NONE;

/// All keyboard shortcuts, in the order of the help.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        keys: "Space",
        description: "Play / pause",
        bindings: &[(NONE, Key::Space, Action::PlayPause)],
    },
    Shortcut {
        keys: "Shift + Space",
        description: "Play backwards / pause (0.5× and slower)",
        bindings: &[(Modifiers::SHIFT, Key::Space, Action::PlayReverse)],
    },
    Shortcut {
        keys: "← / →",
        description: "Previous / next frame",
        bindings: &[
            (NONE, Key::ArrowLeft, Action::Step(-1)),
            (NONE, Key::ArrowRight, Action::Step(1)),
        ],
    },
    Shortcut {
        keys: "Shift + ← / →",
        description: "Back / forward 1 s",
        bindings: &[
            (Modifiers::SHIFT, Key::ArrowLeft, Action::Jump(-1)),
            (Modifiers::SHIFT, Key::ArrowRight, Action::Jump(1)),
        ],
    },
    Shortcut {
        keys: "Ctrl + ← / →",
        description: "Back / forward 10 s (macOS: Cmd)",
        bindings: &[
            (Modifiers::COMMAND, Key::ArrowLeft, Action::Jump(-10)),
            (Modifiers::COMMAND, Key::ArrowRight, Action::Jump(10)),
        ],
    },
    Shortcut {
        keys: "Alt + ← / →",
        description: "Back / forward 60 s",
        bindings: &[
            (Modifiers::ALT, Key::ArrowLeft, Action::Jump(-60)),
            (Modifiers::ALT, Key::ArrowRight, Action::Jump(60)),
        ],
    },
    Shortcut {
        keys: "+ / −",
        description: "Faster / slower (2×, 1×, 0.5×, 0.25×, 0.1×)",
        bindings: &[
            (NONE, Key::Plus, Action::Faster),
            (NONE, Key::Equals, Action::Faster),
            (NONE, Key::Minus, Action::Slower),
        ],
    },
    Shortcut {
        keys: "R",
        description: "Review the recording",
        bindings: &[(NONE, Key::R, Action::Review)],
    },
    Shortcut {
        keys: "L",
        description: "Back to live",
        bindings: &[(NONE, Key::L, Action::Live)],
    },
    Shortcut {
        keys: "M",
        description: "Set marker (live: now, review: shown moment)",
        bindings: &[(NONE, Key::M, Action::SetMarker)],
    },
    Shortcut {
        keys: ", / .",
        description: "Previous / next marker",
        bindings: &[
            (NONE, Key::Comma, Action::PreviousMarker),
            (NONE, Key::Period, Action::NextMarker),
        ],
    },
    Shortcut {
        keys: "1 – 4",
        description: "Camera full screen / back",
        bindings: &[
            (NONE, Key::Num1, Action::FullScreen(0)),
            (NONE, Key::Num2, Action::FullScreen(1)),
            (NONE, Key::Num3, Action::FullScreen(2)),
            (NONE, Key::Num4, Action::FullScreen(3)),
        ],
    },
    Shortcut {
        keys: "Esc",
        description: "Close the help / back to the grid",
        bindings: &[(NONE, Key::Escape, Action::ExitFullScreen)],
    },
    Shortcut {
        keys: "F1",
        description: "Show / hide the keyboard shortcuts",
        bindings: &[(NONE, Key::F1, Action::Help)],
    },
];

/// The help overlay (INP-002): all of [`SHORTCUTS`], as a table.
pub fn help_table(ui: &mut egui::Ui) {
    egui::Grid::new("shortcuts")
        .num_columns(2)
        .spacing([20.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for shortcut in SHORTCUTS {
                ui.label(egui::RichText::new(shortcut.keys).monospace().strong());
                ui.label(shortcut.description);
                ui.end_row();
            }
        });
}

/// Takes the shortcut key presses out of the input, so focused widgets don't react
/// to them as well. Call only when no text field has the keyboard focus.
pub fn read(input: &mut egui::InputState) -> Vec<Action> {
    // The most specific modifier combination first: a key with Shift also matches
    // the binding without modifiers.
    let mut bindings: Vec<&(Modifiers, Key, Action)> =
        SHORTCUTS.iter().flat_map(|s| s.bindings).collect();
    bindings.sort_by_key(|(modifiers, _, _)| std::cmp::Reverse(modifier_count(*modifiers)));
    let mut actions = Vec::new();
    for (modifiers, key, action) in bindings {
        while input.consume_key(*modifiers, *key) {
            actions.push(*action);
        }
    }
    actions
}

fn modifier_count(m: Modifiers) -> u8 {
    u8::from(m.alt)
        + u8::from(m.ctrl)
        + u8::from(m.shift)
        + u8::from(m.mac_cmd)
        + u8::from(m.command)
}

/// Whether an action is meant to have a keyboard shortcut (INP-001). Remote-control
/// conveniences don't need one. A new action must be classified here.
#[cfg(test)]
fn needs_shortcut(action: Action) -> bool {
    match action {
        Action::Pause | Action::Speed(_) => false,
        Action::PlayPause
        | Action::PlayReverse
        | Action::Step(_)
        | Action::Jump(_)
        | Action::Faster
        | Action::Slower
        | Action::Review
        | Action::Live
        | Action::SetMarker
        | Action::PreviousMarker
        | Action::NextMarker
        | Action::FullScreen(_)
        | Action::ExitFullScreen
        | Action::Help => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action, with every value used by the review functions.
    fn all_actions() -> Vec<Action> {
        let mut actions = vec![
            Action::PlayPause,
            Action::PlayReverse,
            Action::Pause,
            Action::Faster,
            Action::Slower,
            Action::Speed(1000),
            Action::Review,
            Action::Live,
            Action::SetMarker,
            Action::PreviousMarker,
            Action::NextMarker,
            Action::ExitFullScreen,
            Action::Help,
        ];
        actions.extend([-1, 1].map(Action::Step));
        actions.extend([-60, -10, -1, 1, 10, 60].map(Action::Jump));
        actions.extend((0..crate::app::MAX_CAMERAS).map(Action::FullScreen));
        actions
    }

    #[test]
    fn every_function_has_a_shortcut_in_the_help() {
        // INP-001, INP-002: the help lists the bindings that `read` uses, so an action
        // with a binding is in the help.
        for action in all_actions().into_iter().filter(|a| needs_shortcut(*a)) {
            assert!(
                SHORTCUTS
                    .iter()
                    .any(|s| s.bindings.iter().any(|(_, _, a)| *a == action)),
                "{action:?} has no shortcut"
            );
        }
        for shortcut in SHORTCUTS {
            assert!(!shortcut.keys.is_empty() && !shortcut.description.is_empty());
            assert!(
                !shortcut.bindings.is_empty(),
                "{} has no binding",
                shortcut.keys
            );
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let bindings: Vec<_> = SHORTCUTS
            .iter()
            .flat_map(|s| s.bindings)
            .map(|(m, k, _)| (*m, *k))
            .collect();
        for (i, binding) in bindings.iter().enumerate() {
            assert!(
                !bindings[i + 1..].contains(binding),
                "{binding:?} is bound twice"
            );
        }
    }

    #[test]
    fn modifiers_take_precedence() {
        let ctx = egui::Context::default();
        let press = |modifiers: Modifiers, key: Key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let raw = egui::RawInput {
            events: vec![
                press(Modifiers::SHIFT, Key::Space),
                press(Modifiers::ALT, Key::ArrowLeft),
                press(NONE, Key::ArrowRight),
            ],
            ..Default::default()
        };
        let mut actions = Vec::new();
        let _ = ctx.run(raw, |ctx| actions = ctx.input_mut(read));
        actions.sort_by_key(|a| format!("{a:?}"));
        assert_eq!(
            actions,
            vec![Action::Jump(-60), Action::PlayReverse, Action::Step(1)]
        );
    }
}
