//! Synthetic guest channel registry. Destroy must close every open channel.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRegistry {
    open: BTreeSet<String>,
    destroyed: BTreeSet<String>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            open: BTreeSet::new(),
            destroyed: BTreeSet::new(),
        }
    }

    pub fn open_channel(&mut self, channel_id: impl Into<String>) -> HarnessResult<()> {
        let channel_id = channel_id.into();
        if self.destroyed.contains(&channel_id) {
            return Err(HarnessError::invalid_state(
                "cannot reopen a destroyed channel",
            ));
        }
        self.open.insert(channel_id);
        Ok(())
    }

    pub fn destroy_channel(&mut self, channel_id: impl Into<String>) -> HarnessResult<()> {
        let channel_id = channel_id.into();
        if !self.open.remove(&channel_id) {
            return Err(HarnessError::invalid_state("channel was not open"));
        }
        self.destroyed.insert(channel_id);
        Ok(())
    }

    pub fn destroy_all(&mut self) -> HarnessResult<()> {
        let pending = self.open.clone();
        for channel_id in pending {
            self.destroy_channel(channel_id)?;
        }
        Ok(())
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    pub fn assert_all_destroyed(&self) -> HarnessResult<()> {
        if !self.open.is_empty() {
            return Err(HarnessError::channel_leak(format!(
                "{} channel(s) still open after destroy",
                self.open.len()
            )));
        }
        Ok(())
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}
