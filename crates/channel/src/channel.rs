mod channel_buffer;
mod channel_chat;
mod channel_store;

use client::{GlobalClient, GlobalUserStore};
use gpui::App;

pub use channel_buffer::{ACKNOWLEDGE_DEBOUNCE_INTERVAL, ChannelBuffer, ChannelBufferEvent};
pub use channel_chat::{
    ChannelChat, ChannelChatEvent, ChannelMessage, ChannelMessageId, MessageParams,
    mentions_to_proto,
};
pub use channel_store::{Channel, ChannelEvent, ChannelMembership, ChannelStore};

#[cfg(test)]
mod channel_store_tests;

pub fn init(cx: &mut App) {
    let user_store = cx.global::<GlobalUserStore>().0.clone();
    let client = cx.global::<GlobalClient>().0.clone();
    channel_store::init(user_store, cx);
    channel_buffer::init(&client.clone().into());
    channel_chat::init(&client.clone().into());
}

// pub fn init(user_store: Entity<UserStore>, cx: &mut App) {
//     let client = cx.global::<GlobalClient>().0.clone();
//     channel_store::init(user_store, cx);
//     channel_buffer::init(client.clone().into());
//     channel_chat::init(client.into());
// }
