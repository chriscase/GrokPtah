//! Efficiency budgets. Safety is not parameterized by profile.

use crate::types::{ActionClass, ProfileId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileBudget {
    pub allow_screenshot: bool,
    pub allow_pointer: bool,
    pub allow_key_chord: bool,
    pub include_element_bounds: bool,
    pub max_observation_elements: usize,
    pub max_image_bytes: u64,
    pub max_steps: u32,
    pub virtual_observe_cost: u64,
}

impl ProfileBudget {
    pub fn for_profile(profile: ProfileId) -> Self {
        match profile {
            ProfileId::Economy => Self {
                allow_screenshot: false,
                allow_pointer: false,
                allow_key_chord: false,
                include_element_bounds: false,
                max_observation_elements: 8,
                max_image_bytes: 0,
                max_steps: 8,
                virtual_observe_cost: 1,
            },
            ProfileId::Balanced => Self {
                allow_screenshot: true,
                allow_pointer: true,
                allow_key_chord: false,
                include_element_bounds: true,
                max_observation_elements: 16,
                max_image_bytes: 4_096,
                max_steps: 10,
                virtual_observe_cost: 3,
            },
            ProfileId::HighAssurance => Self {
                allow_screenshot: true,
                allow_pointer: true,
                allow_key_chord: true,
                include_element_bounds: true,
                max_observation_elements: 24,
                max_image_bytes: 16_384,
                max_steps: 12,
                virtual_observe_cost: 5,
            },
        }
    }

    pub fn allows_class(self, class: ActionClass) -> bool {
        match class {
            ActionClass::Semantic | ActionClass::TextEntry => true,
            ActionClass::PointerFallback => self.allow_pointer,
            ActionClass::KeyChord => self.allow_key_chord,
        }
    }
}
