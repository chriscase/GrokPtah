use super::isolated_visual::IsolatedVisualResourceLimits;
use super::types::{
    ComputerError, ComputerErrorCode, ComputerKey, ComputerResult, PointerButton,
    PointerButtonState,
};

pub const ISOLATED_VISUAL_MAX_SCROLL_DELTA: i32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolatedVisualInputKeyState {
    Down,
    Up,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolatedVisualInputMessage {
    PointerMove {
        x: u32,
        y: u32,
    },
    PointerButton {
        x: u32,
        y: u32,
        button: PointerButton,
        state: PointerButtonState,
    },
    Scroll {
        delta_x: i32,
        delta_y: i32,
    },
    Key {
        key: ComputerKey,
        state: IsolatedVisualInputKeyState,
    },
    Text {
        text: String,
    },
}

/// Host-side admission state for guest-local input. This is deliberately
/// independent from the existing read-only protocol session: enabling this
/// gate still requires a packaged backend proof and a separate reviewed
/// dispatch path. It prevents a future carrier from inventing implicit
/// releases, bypassing frame freshness, or replaying an accepted event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedVisualInputGate {
    limits: IsolatedVisualResourceLimits,
    frame_sequence: u64,
    width: u32,
    height: u32,
    next_input_sequence: u64,
    accepted_events: u32,
    held_button: Option<PointerButton>,
    pressed_keys: Vec<ComputerKey>,
    poisoned: bool,
}

impl IsolatedVisualInputGate {
    pub fn new(limits: IsolatedVisualResourceLimits) -> ComputerResult<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            frame_sequence: 0,
            width: 0,
            height: 0,
            next_input_sequence: 0,
            accepted_events: 0,
            held_button: None,
            pressed_keys: Vec::new(),
            poisoned: false,
        })
    }

    pub fn bind_frame(
        &mut self,
        frame_sequence: u64,
        width: u32,
        height: u32,
    ) -> ComputerResult<()> {
        self.ensure_live()?;
        if frame_sequence == 0 || frame_sequence <= self.frame_sequence {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated input frame sequence is stale",
            ));
        }
        self.validate_dimensions(width, height)?;
        self.frame_sequence = frame_sequence;
        self.width = width;
        self.height = height;
        Ok(())
    }

    pub fn admit(
        &mut self,
        frame_sequence: u64,
        input_sequence: u64,
        message: IsolatedVisualInputMessage,
    ) -> ComputerResult<()> {
        self.ensure_live()?;
        if self.frame_sequence == 0 || frame_sequence != self.frame_sequence {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated input is not bound to the latest frame",
            ));
        }
        let expected = self.next_input_sequence.checked_add(1).ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated input sequence exhausted",
            )
        })?;
        if input_sequence != expected {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "isolated input sequence is duplicate, skipped, or stale",
            ));
        }
        if self.accepted_events >= self.limits.input_events {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated input event limit reached",
            ));
        }
        self.validate_message(&message)?;
        self.apply_message(&message)?;
        self.next_input_sequence = input_sequence;
        self.accepted_events += 1;
        Ok(())
    }

    pub fn poison(&mut self) {
        self.poisoned = true;
    }

    pub fn terminal_check(&self) -> ComputerResult<()> {
        if self.poisoned {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated input gate is poisoned",
            ));
        }
        if self.held_button.is_some() || !self.pressed_keys.is_empty() {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "isolated input cannot terminate with a pressed key or button",
            ));
        }
        Ok(())
    }

    pub fn frame_sequence(&self) -> u64 {
        self.frame_sequence
    }

    pub fn next_input_sequence(&self) -> u64 {
        self.next_input_sequence
    }

    pub fn accepted_events(&self) -> u32 {
        self.accepted_events
    }

    fn ensure_live(&self) -> ComputerResult<()> {
        if self.poisoned {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "isolated input gate is poisoned",
            ));
        }
        Ok(())
    }

    fn validate_message(&self, message: &IsolatedVisualInputMessage) -> ComputerResult<()> {
        match message {
            IsolatedVisualInputMessage::PointerMove { x, y }
            | IsolatedVisualInputMessage::PointerButton { x, y, .. } => self.validate_point(*x, *y),
            IsolatedVisualInputMessage::Scroll { delta_x, delta_y }
                if delta_x.unsigned_abs() > ISOLATED_VISUAL_MAX_SCROLL_DELTA as u32
                    || delta_y.unsigned_abs() > ISOLATED_VISUAL_MAX_SCROLL_DELTA as u32 =>
            {
                Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "isolated scroll delta exceeds its bound",
                ))
            }
            IsolatedVisualInputMessage::Key { .. } => Ok(()),
            IsolatedVisualInputMessage::Scroll { .. } => Ok(()),
            IsolatedVisualInputMessage::Text { text } => {
                if text.is_empty() {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidRequest,
                        "isolated text input is empty",
                    ));
                }
                if text.len() > self.limits.text_event_bytes as usize {
                    return Err(ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "isolated text input exceeds its bound",
                    ));
                }
                if text.contains('\0') {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidRequest,
                        "isolated text input contains a null byte",
                    ));
                }
                Ok(())
            }
        }
    }

    fn validate_point(&self, x: u32, y: u32) -> ComputerResult<()> {
        if self.frame_sequence == 0 || x >= self.width || y >= self.height {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenTarget,
                "isolated input point is outside the measured display",
            ));
        }
        Ok(())
    }

    fn validate_dimensions(&self, width: u32, height: u32) -> ComputerResult<()> {
        if width == 0
            || width > self.limits.display_width
            || height == 0
            || height > self.limits.display_height
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "isolated frame dimensions exceed the measured manifest",
            ));
        }
        Ok(())
    }

    fn apply_message(&mut self, message: &IsolatedVisualInputMessage) -> ComputerResult<()> {
        match message {
            IsolatedVisualInputMessage::PointerMove { .. }
            | IsolatedVisualInputMessage::Scroll { .. }
            | IsolatedVisualInputMessage::Text { .. } => Ok(()),
            IsolatedVisualInputMessage::PointerButton { button, state, .. } => match state {
                PointerButtonState::Down if self.held_button.is_none() => {
                    self.held_button = Some(*button);
                    Ok(())
                }
                PointerButtonState::Up if self.held_button == Some(*button) => {
                    self.held_button = None;
                    Ok(())
                }
                PointerButtonState::Down => Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated pointer button is already held",
                )),
                PointerButtonState::Up => Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated pointer button release does not match the held button",
                )),
            },
            IsolatedVisualInputMessage::Key { key, state } => match state {
                IsolatedVisualInputKeyState::Down if !self.pressed_keys.contains(key) => {
                    self.pressed_keys.push(*key);
                    Ok(())
                }
                IsolatedVisualInputKeyState::Up if self.pressed_keys.contains(key) => {
                    self.pressed_keys.retain(|pressed| pressed != key);
                    Ok(())
                }
                IsolatedVisualInputKeyState::Down => Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated key is already held",
                )),
                IsolatedVisualInputKeyState::Up => Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "isolated key release does not match a held key",
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> IsolatedVisualInputGate {
        IsolatedVisualInputGate::new(IsolatedVisualResourceLimits::proof_defaults()).unwrap()
    }

    #[test]
    fn pointer_drag_requires_explicit_down_and_up() {
        let mut gate = gate();
        gate.bind_frame(1, 800, 600).unwrap();
        gate.admit(
            1,
            1,
            IsolatedVisualInputMessage::PointerButton {
                x: 12,
                y: 20,
                button: PointerButton::Primary,
                state: PointerButtonState::Down,
            },
        )
        .unwrap();
        assert_eq!(
            gate.admit(
                1,
                2,
                IsolatedVisualInputMessage::PointerButton {
                    x: 12,
                    y: 20,
                    button: PointerButton::Primary,
                    state: PointerButtonState::Down,
                },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::Conflict
        );
        gate.admit(
            1,
            2,
            IsolatedVisualInputMessage::PointerMove { x: 20, y: 40 },
        )
        .unwrap();
        gate.admit(
            1,
            3,
            IsolatedVisualInputMessage::PointerButton {
                x: 20,
                y: 40,
                button: PointerButton::Primary,
                state: PointerButtonState::Up,
            },
        )
        .unwrap();
        gate.terminal_check().unwrap();
    }

    #[test]
    fn frame_and_sequence_fences_reject_replay_and_stale_points() {
        let mut gate = gate();
        gate.bind_frame(3, 800, 600).unwrap();
        assert_eq!(
            gate.admit(2, 1, IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },)
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
        assert_eq!(
            gate.admit(3, 2, IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 },)
                .unwrap_err()
                .code,
            ComputerErrorCode::StaleObservation
        );
        assert_eq!(
            gate.admit(
                3,
                1,
                IsolatedVisualInputMessage::PointerMove { x: 800, y: 1 },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::ForbiddenTarget
        );
        gate.admit(3, 1, IsolatedVisualInputMessage::PointerMove { x: 1, y: 1 })
            .unwrap();
    }

    #[test]
    fn key_and_text_limits_are_closed() {
        let mut gate = gate();
        gate.bind_frame(1, 800, 600).unwrap();
        gate.admit(
            1,
            1,
            IsolatedVisualInputMessage::Key {
                key: ComputerKey::Shift,
                state: IsolatedVisualInputKeyState::Down,
            },
        )
        .unwrap();
        assert_eq!(
            gate.admit(
                1,
                2,
                IsolatedVisualInputMessage::Key {
                    key: ComputerKey::Shift,
                    state: IsolatedVisualInputKeyState::Down,
                },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::Conflict
        );
        assert_eq!(
            gate.admit(
                1,
                2,
                IsolatedVisualInputMessage::Text {
                    text: "x".repeat(4097),
                },
            )
            .unwrap_err()
            .code,
            ComputerErrorCode::LimitReached
        );
        gate.admit(
            1,
            2,
            IsolatedVisualInputMessage::Key {
                key: ComputerKey::Shift,
                state: IsolatedVisualInputKeyState::Up,
            },
        )
        .unwrap();
        gate.terminal_check().unwrap();
    }
}
