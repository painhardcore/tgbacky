use crate::error::{AppError, Result};
use crate::media::{
    classify_document, classify_photo, stable_media_key_for_document, stable_media_key_for_photo,
};
use crate::pacing::PaceBucket;
use crate::telegram::{RealMediaHandle, RealTelegramGateway, TelegramGateway};
use crate::types::{ChatKind, ChatSummary, MediaDescriptor, ScannedMessage};
use grammers_client::InvocationError;
use grammers_client::media::Media;
use grammers_client::message::Message;
use grammers_client::peer::Peer;
use grammers_session::types::PeerRef;

pub(super) fn chat_summary_from_peer(peer: &Peer, handle: PeerRef) -> ChatSummary<PeerRef> {
    let kind = match peer {
        Peer::User(_) => ChatKind::User,
        Peer::Group(group) if group.is_megagroup() => ChatKind::Supergroup,
        Peer::Group(_) => ChatKind::Group,
        Peer::Channel(_) => ChatKind::Channel,
    };

    ChatSummary {
        id: peer.id().bot_api_dialog_id(),
        title: peer.name().unwrap_or_default().to_string(),
        username: peer.username().map(ToString::to_string),
        kind,
        handle,
    }
}

pub(super) fn map_message(message: Message) -> ScannedMessage<RealMediaHandle> {
    let mut media = Vec::new();
    if let Some(message_media) = message.media() {
        match message_media {
            Media::Photo(photo) => {
                media.push(MediaDescriptor {
                    kind: classify_photo(&photo),
                    telegram_media_key: stable_media_key_for_photo(&photo),
                    mime_type: Some("image/jpeg".to_string()),
                    file_size_bytes: photo.size().map(|size| size as i64),
                    original_name: None,
                    handle: RealMediaHandle::Photo(photo),
                });
            }
            Media::Document(document) => {
                if let Some(kind) = classify_document(&document) {
                    media.push(MediaDescriptor {
                        kind,
                        telegram_media_key: stable_media_key_for_document(&document),
                        mime_type: document.mime_type().map(ToString::to_string),
                        file_size_bytes: document.size().map(|size| size as i64),
                        original_name: document.name().map(ToString::to_string),
                        handle: RealMediaHandle::Document(document),
                    });
                }
            }
            Media::Sticker(_) => {}
            _ => {}
        }
    }

    ScannedMessage {
        message_id: message.id(),
        date: message.date(),
        media,
    }
}

pub(super) async fn list_chats_impl(
    gateway: &RealTelegramGateway,
) -> Result<Vec<ChatSummary<PeerRef>>> {
    if !gateway.is_authorized().await? {
        return Err(AppError::Authentication(
            "session is not authorized; run `tgbacky auth` first".to_string(),
        ));
    }

    let mut dialogs = gateway.client.iter_dialogs();
    let mut chats = Vec::new();
    let mut attempt = 0_u32;
    loop {
        gateway.pacer.wait_for_turn(PaceBucket::Request).await;
        match dialogs.next().await {
            Ok(Some(dialog)) => {
                attempt = 0;
                chats.push(chat_summary_from_peer(dialog.peer(), dialog.peer_ref()));
            }
            Ok(None) => break,
            Err(InvocationError::Rpc(error)) if error.code == 420 => {
                let seconds = error.value.unwrap_or(0) as i32;
                gateway
                    .pacer
                    .sleep_on_flood_wait("list chats", seconds, attempt)
                    .await?;
                attempt += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }

    chats.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(chats)
}

pub(super) async fn resolve_chat_impl(
    gateway: &RealTelegramGateway,
    query: &str,
) -> Result<ChatSummary<PeerRef>> {
    if query.starts_with("http://") || query.starts_with("https://") {
        let parsed = url::Url::parse(query)
            .map_err(|_| AppError::ChatResolution("invalid invite or chat URL".to_string()))?;
        if parsed
            .domain()
            .is_some_and(|domain| domain.contains("t.me"))
        {
            return Err(AppError::Unsupported(
                "invite links are not implemented in this v1 build".to_string(),
            ));
        }
    }

    if query.starts_with('@') {
        let username = query.trim_start_matches('@').trim().to_string();
        let peer = gateway
            .invoke_with_policy(PaceBucket::Request, "resolve username", || {
                gateway.client.resolve_username(&username)
            })
            .await?
            .ok_or_else(|| AppError::ChatResolution(format!("no chat found for @{username}")))?;
        let handle = peer
            .to_ref()
            .await
            .ok_or_else(|| AppError::ChatResolution("resolved peer is not usable".to_string()))?;
        return Ok(chat_summary_from_peer(&peer, handle));
    }

    let chats = list_chats_impl(gateway).await?;
    if let Ok(dialog_id) = query.trim().parse::<i64>() {
        return chats
            .into_iter()
            .find(|chat| chat.id == dialog_id)
            .ok_or_else(|| AppError::ChatResolution(format!("chat id {dialog_id} not found")));
    }

    let matches = chats
        .into_iter()
        .filter(|chat| chat.title == query)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::ChatResolution(format!(
            "no chat with exact title `{query}` found"
        ))),
        [chat] => Ok(chat.clone()),
        _ => Err(AppError::ChatResolution(format!(
            "multiple chats share the title `{query}`; use @username or numeric id"
        ))),
    }
}

pub(super) async fn fetch_history_batch_impl(
    gateway: &RealTelegramGateway,
    chat: &PeerRef,
    offset_id: Option<i32>,
    batch_size: usize,
) -> Result<Vec<ScannedMessage<RealMediaHandle>>> {
    let mut iterator = gateway.client.iter_messages(*chat).limit(batch_size);
    if let Some(offset_id) = offset_id {
        iterator = iterator.offset_id(offset_id);
    }

    let mut messages = Vec::new();
    let mut attempt = 0_u32;
    // `iter_messages(limit)` buffers a page of history internally. Pacing every
    // `next()` artificially adds request delay to buffered items too, which makes
    // long history scans much slower than necessary. We pace before the batch
    // starts and rely on FLOOD_WAIT handling if Telegram asks us to cool down.
    gateway.pacer.wait_for_turn(PaceBucket::Request).await;
    loop {
        match iterator.next().await {
            Ok(Some(message)) => {
                attempt = 0;
                messages.push(map_message(message));
            }
            Ok(None) => break,
            Err(InvocationError::Rpc(error)) if error.code == 420 => {
                let seconds = error.value.unwrap_or(0) as i32;
                gateway
                    .pacer
                    .sleep_on_flood_wait("fetch history", seconds, attempt)
                    .await?;
                gateway.pacer.wait_for_turn(PaceBucket::Request).await;
                attempt += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(messages)
}

pub(super) async fn fetch_messages_by_ids_impl(
    gateway: &RealTelegramGateway,
    chat: &PeerRef,
    message_ids: &[i32],
) -> Result<Vec<ScannedMessage<RealMediaHandle>>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }

    let messages = gateway
        .invoke_with_policy(PaceBucket::Request, "fetch messages by id", || {
            gateway.client.get_messages_by_id(*chat, message_ids)
        })
        .await?;

    Ok(messages
        .into_iter()
        .flatten()
        .map(map_message)
        .collect::<Vec<_>>())
}
