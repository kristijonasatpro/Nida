//! A channel's name and its place in the row: renamed and moved from the
//! Broadcast layout's tiles, a channel's right-click menu, or the API.

use crate::engine::{CommandResult, ErrorCode};

use super::super::VardaApp;

/// Longest a channel name can be; the tiles have room for about this much.
const MAX_NAME: usize = 40;

impl VardaApp {
    /// Call a channel something else.
    pub fn cmd_rename_channel(&mut self, channel_uuid: &str, name: &str) -> CommandResult {
        let Some(idx) = self.mixer.find_channel_by_uuid(channel_uuid) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No channel {channel_uuid}"),
            };
        };
        let name = name.trim();
        if name.is_empty() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "A channel needs a name".into(),
            };
        }
        if let Some(ch) = self.mixer.channel_mut(idx) {
            ch.name = name.chars().take(MAX_NAME).collect();
        }
        CommandResult::Ok
    }

    /// Move a channel to just before another, or to the end. A later channel
    /// is drawn over an earlier one in the layered mixer, and it is the order
    /// the tiles, columns and rows show.
    pub fn cmd_move_channel(
        &mut self,
        channel_uuid: &str,
        before_channel_uuid: Option<&str>,
    ) -> CommandResult {
        let Some(from) = self.mixer.find_channel_by_uuid(channel_uuid) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No channel {channel_uuid}"),
            };
        };
        if before_channel_uuid == Some(channel_uuid) {
            return CommandResult::Ok;
        }
        if let Some(before) = before_channel_uuid {
            if self.mixer.find_channel_by_uuid(before).is_none() {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("No channel {before}"),
                };
            }
        }
        let channels = self.mixer.channels_mut();
        let channel = channels.remove(from);
        let to = before_channel_uuid
            .and_then(|before| channels.iter().position(|c| c.uuid() == before))
            .unwrap_or(channels.len());
        channels.insert(to, channel);
        CommandResult::Ok
    }
}
