use std::{fmt, str::FromStr, time::Duration};

use serde::{de, Deserialize, Deserializer, Serialize};

/// Side-specific values are important: macOS reports the physical left and
/// right modifier keys independently, and the setting must survive a restart
/// without being collapsed to a generic accelerator modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierKey {
    Fn,
    LeftOption,
    RightOption,
    LeftCommand,
    RightCommand,
    LeftShift,
    RightShift,
    LeftControl,
    RightControl,
}

impl ModifierKey {
    pub const ALL: [Self; 9] = [
        Self::Fn,
        Self::LeftOption,
        Self::RightOption,
        Self::LeftCommand,
        Self::RightCommand,
        Self::LeftShift,
        Self::RightShift,
        Self::LeftControl,
        Self::RightControl,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fn => "fn",
            Self::LeftOption => "left_option",
            Self::RightOption => "right_option",
            Self::LeftCommand => "left_command",
            Self::RightCommand => "right_command",
            Self::LeftShift => "left_shift",
            Self::RightShift => "right_shift",
            Self::LeftControl => "left_control",
            Self::RightControl => "right_control",
        }
    }
}

impl fmt::Display for ModifierKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ModifierKey {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "fn" => Ok(Self::Fn),
            "left_option" => Ok(Self::LeftOption),
            "right_option" => Ok(Self::RightOption),
            "left_command" => Ok(Self::LeftCommand),
            "right_command" => Ok(Self::RightCommand),
            "left_shift" => Ok(Self::LeftShift),
            "right_shift" => Ok(Self::RightShift),
            "left_control" => Ok(Self::LeftControl),
            "right_control" => Ok(Self::RightControl),
            _ => Err(()),
        }
    }
}

impl<'de> Deserialize<'de> for ModifierKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(|()| {
            de::Error::unknown_variant(
                &value,
                &[
                    "fn",
                    "left_option",
                    "right_option",
                    "left_command",
                    "right_command",
                    "left_shift",
                    "right_shift",
                    "left_control",
                    "right_control",
                ],
            )
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierConfig {
    pub inline_key: Option<ModifierKey>,
    pub hold_to_talk_key: Option<ModifierKey>,
    pub double_tap_interval: Duration,
    pub maximum_tap_duration: Duration,
    pub hold_delay: Duration,
}

impl Default for ModifierConfig {
    fn default() -> Self {
        Self {
            inline_key: Some(ModifierKey::RightOption),
            hold_to_talk_key: Some(ModifierKey::Fn),
            double_tap_interval: Duration::from_millis(360),
            maximum_tap_duration: Duration::from_millis(210),
            hold_delay: Duration::from_millis(240),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierInput {
    pub key: ModifierKey,
    pub pressed: bool,
    pub other_modifiers: bool,
    pub at: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierEvent {
    InlineInvoked,
    HoldToTalkStarted,
    HoldToTalkReleased,
    Cancelled,
    SecureInputRefused,
    PermissionRefused,
}

#[derive(Clone, Debug)]
pub struct ModifierStateMachine {
    config: ModifierConfig,
    press: Option<PressState>,
    last_tap: Option<TapState>,
}

#[derive(Clone, Copy, Debug)]
struct PressState {
    key: ModifierKey,
    started_at: Duration,
    eligible: bool,
    hold_started: bool,
}

#[derive(Clone, Copy, Debug)]
struct TapState {
    key: ModifierKey,
    released_at: Duration,
}

impl ModifierStateMachine {
    pub fn new(config: ModifierConfig) -> Self {
        Self {
            config,
            press: None,
            last_tap: None,
        }
    }

    pub fn handle(&mut self, input: ModifierInput) -> Vec<ModifierEvent> {
        if input.pressed {
            return self.press(input);
        }
        self.release(input)
    }

    pub fn poll(&mut self, now: Duration) -> Vec<ModifierEvent> {
        let Some(press) = self.press.as_mut() else {
            return Vec::new();
        };
        if press.eligible
            && !press.hold_started
            && self.config.hold_to_talk_key == Some(press.key)
            && now.saturating_sub(press.started_at) >= self.config.hold_delay
        {
            press.hold_started = true;
            self.last_tap = None;
            return vec![ModifierEvent::HoldToTalkStarted];
        }
        Vec::new()
    }

    pub fn cancel(&mut self) -> Vec<ModifierEvent> {
        let active_hold = self.press.is_some_and(|press| press.hold_started);
        self.press = None;
        self.last_tap = None;
        if active_hold {
            vec![ModifierEvent::Cancelled]
        } else {
            Vec::new()
        }
    }

    fn press(&mut self, input: ModifierInput) -> Vec<ModifierEvent> {
        if self.press.is_some_and(|press| press.key == input.key) {
            return Vec::new();
        }
        let events = if self.press.is_some() {
            self.cancel()
        } else {
            Vec::new()
        };
        let bound = self.config.inline_key == Some(input.key)
            || self.config.hold_to_talk_key == Some(input.key);
        if input.other_modifiers || !bound {
            self.last_tap = None;
        }
        if bound {
            self.press = Some(PressState {
                key: input.key,
                started_at: input.at,
                eligible: !input.other_modifiers,
                hold_started: false,
            });
        }
        events
    }

    fn release(&mut self, input: ModifierInput) -> Vec<ModifierEvent> {
        let Some(mut press) = self.press.take() else {
            return Vec::new();
        };
        if press.key != input.key {
            self.press = Some(press);
            return Vec::new();
        }
        let held_for = input.at.saturating_sub(press.started_at);
        let hold_was_started = press.hold_started;
        if press.eligible
            && !press.hold_started
            && self.config.hold_to_talk_key == Some(press.key)
            && held_for >= self.config.hold_delay
        {
            press.hold_started = true;
        }
        if press.hold_started {
            self.last_tap = None;
            return if hold_was_started {
                vec![ModifierEvent::HoldToTalkReleased]
            } else {
                vec![
                    ModifierEvent::HoldToTalkStarted,
                    ModifierEvent::HoldToTalkReleased,
                ]
            };
        }
        if !press.eligible
            || input.other_modifiers
            || held_for > self.config.maximum_tap_duration
            || self.config.inline_key != Some(press.key)
        {
            self.last_tap = None;
            return Vec::new();
        }

        // A shared modifier uses press duration: a short press invokes inline
        // assistance while a long press takes the hold-to-talk branch above.
        if self.config.hold_to_talk_key == Some(press.key) {
            self.last_tap = None;
            return vec![ModifierEvent::InlineInvoked];
        }

        if let Some(previous) = self.last_tap {
            let within_window =
                input.at.saturating_sub(previous.released_at) <= self.config.double_tap_interval;
            if within_window && previous.key == press.key {
                self.last_tap = None;
                return vec![ModifierEvent::InlineInvoked];
            }
        }
        self.last_tap = Some(TapState {
            key: press.key,
            released_at: input.at,
        });
        Vec::new()
    }
}

impl Default for ModifierStateMachine {
    fn default() -> Self {
        Self::new(ModifierConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(key: ModifierKey, pressed: bool, milliseconds: u64) -> ModifierInput {
        ModifierInput {
            key,
            pressed,
            other_modifiers: false,
            at: Duration::from_millis(milliseconds),
        }
    }

    #[test]
    fn exposes_the_exact_modifier_vocabulary() {
        assert_eq!(
            ModifierKey::ALL.map(ModifierKey::as_str),
            [
                "fn",
                "left_option",
                "right_option",
                "left_command",
                "right_command",
                "left_shift",
                "right_shift",
                "left_control",
                "right_control",
            ]
        );
        for key in ModifierKey::ALL {
            assert_eq!(key.as_str().parse::<ModifierKey>(), Ok(key));
        }
    }

    #[test]
    fn rejects_noncanonical_modifier_spellings() {
        assert!("option".parse::<ModifierKey>().is_err());
        assert!("left-option".parse::<ModifierKey>().is_err());
        assert!("right-alt".parse::<ModifierKey>().is_err());
    }

    #[test]
    fn emits_inline_event_for_two_quick_modifier_taps() {
        let mut state = ModifierStateMachine::default();
        assert!(state
            .handle(input(ModifierKey::RightOption, true, 0))
            .is_empty());
        assert!(state
            .handle(input(ModifierKey::RightOption, false, 60))
            .is_empty());
        assert!(state
            .handle(input(ModifierKey::RightOption, true, 180))
            .is_empty());
        assert_eq!(
            state.handle(input(ModifierKey::RightOption, false, 230)),
            vec![ModifierEvent::InlineInvoked]
        );
    }

    #[test]
    fn fn_hold_has_distinct_start_and_release() {
        let mut state = ModifierStateMachine::default();
        state.handle(input(ModifierKey::Fn, true, 0));
        assert!(state.poll(Duration::from_millis(200)).is_empty());
        assert_eq!(
            state.poll(Duration::from_millis(240)),
            vec![ModifierEvent::HoldToTalkStarted]
        );
        assert_eq!(
            state.handle(input(ModifierKey::Fn, false, 500)),
            vec![ModifierEvent::HoldToTalkReleased]
        );
    }

    #[test]
    fn shared_modifier_short_press_stays_inline_and_long_press_dictates() {
        let mut state = ModifierStateMachine::new(ModifierConfig {
            inline_key: Some(ModifierKey::RightOption),
            hold_to_talk_key: Some(ModifierKey::RightOption),
            ..ModifierConfig::default()
        });
        state.handle(input(ModifierKey::RightOption, true, 0));
        assert_eq!(
            state.handle(input(ModifierKey::RightOption, false, 70)),
            vec![ModifierEvent::InlineInvoked]
        );

        state.handle(input(ModifierKey::RightOption, true, 300));
        assert_eq!(
            state.poll(Duration::from_millis(540)),
            vec![ModifierEvent::HoldToTalkStarted]
        );
        assert_eq!(
            state.handle(input(ModifierKey::RightOption, false, 700)),
            vec![ModifierEvent::HoldToTalkReleased]
        );
    }

    #[test]
    fn a_different_bound_modifier_does_not_trigger_inline() {
        let mut state = ModifierStateMachine::default();
        state.handle(input(ModifierKey::LeftOption, true, 0));
        state.handle(input(ModifierKey::LeftOption, false, 50));
        state.handle(input(ModifierKey::LeftOption, true, 100));
        assert!(state
            .handle(input(ModifierKey::LeftOption, false, 150))
            .is_empty());
    }

    #[test]
    fn other_modifiers_disqualify_a_tap() {
        let mut state = ModifierStateMachine::default();
        let mut modified = input(ModifierKey::RightOption, true, 0);
        modified.other_modifiers = true;
        state.handle(modified);
        modified.pressed = false;
        modified.at = Duration::from_millis(50);
        state.handle(modified);
        state.handle(input(ModifierKey::RightOption, true, 100));
        assert!(state
            .handle(input(ModifierKey::RightOption, false, 150))
            .is_empty());
    }

    #[test]
    fn cancellation_cleans_up_an_active_hold_once() {
        let mut state = ModifierStateMachine::default();
        state.handle(input(ModifierKey::Fn, true, 0));
        state.poll(Duration::from_millis(250));
        assert_eq!(state.cancel(), vec![ModifierEvent::Cancelled]);
        assert!(state.cancel().is_empty());
    }
}
