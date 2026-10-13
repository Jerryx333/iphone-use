//! Uses idevice's DTX framing; one reader owns the byte stream throughout its lifetime.
use crate::{ConnectionState, DEADLINE, Error, Event, Result};
use idevice::{
    ReadWrite,
    dvt::message::{Aux, AuxValue, Message, MessageHeader, PayloadHeader},
};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

type Reply = oneshot::Sender<Result<Message>>;
struct Call {
    channel: i32,
    selector: String,
    args: Vec<AuxValue>,
    reply: Reply,
}
pub(crate) struct Dtx {
    commands: mpsc::Sender<Call>,
    tasks: Vec<JoinHandle<()>>,
}
fn u32_at(bytes: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(bytes[n..n + 4].try_into().unwrap())
}
// Public header serialization exposes the fixed DTX header layout, even though accessors are private.
fn metadata(msg: &Message) -> (u32, u32, bool, u8) {
    let h = msg.message_header.serialize();
    (
        u32_at(&h, 16),
        u32_at(&h, 20),
        u32_at(&h, 28) != 0,
        msg.payload_header.serialize()[0],
    )
}
fn frame(channel: i32, id: u32, selector: &str, args: Vec<AuxValue>, expects: bool) -> Vec<u8> {
    Message::new(
        MessageHeader::new(0, 1, id, 0, channel, expects),
        PayloadHeader::method_invocation(),
        Some(Aux::from_values(args)),
        Some(selector.into()),
    )
    .serialize()
}
fn ack(msg: &Message) -> Vec<u8> {
    let (id, conversation, _, _) = metadata(msg);
    let mut bytes = Message::new(
        MessageHeader::new(
            0,
            1,
            id,
            conversation + 1,
            -msg.message_header.channel,
            false,
        ),
        PayloadHeader::new(),
        None,
        None,
    )
    .serialize();
    bytes[32] = 0; // DTX OK, no return object.
    bytes
}
impl Dtx {
    pub(crate) async fn open(
        stream: Box<dyn ReadWrite>,
        label: &'static str,
        events: broadcast::Sender<Event>,
        state: Arc<watch::Sender<ConnectionState>>,
    ) -> Result<Self> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (incoming_tx, mut incoming) = mpsc::channel(128);
        let reader_task = tokio::spawn(async move {
            loop {
                let result = Message::from_reader(&mut reader).await.map_err(Error::from);
                let failed = result.is_err();
                if incoming_tx.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        let (commands, mut rx) = mpsc::channel::<Call>(16);
        let (hello_tx, hello_rx) = oneshot::channel();
        let actor = tokio::spawn(async move {
            let mut pending: HashMap<u32, Reply> = HashMap::new();
            let mut next_id = 1u32;
            let mut hello = Some(hello_tx);
            let mut caps = plist::Dictionary::new();
            caps.insert("com.apple.private.DTXBlockCompression".into(), 0i64.into());
            caps.insert("com.apple.private.DTXConnection".into(), 1i64.into());
            let run: Result<()> = async {
                writer.write_all(&frame(0,next_id,"_notifyOfPublishedCapabilities:",vec![AuxValue::archived_value(plist::Value::Dictionary(caps))],false)).await?;
                loop {
                    tokio::select! {
                        command = rx.recv() => {
                            let Some(call) = command else { return Ok(()); };
                            next_id = next_id.checked_add(1).ok_or_else(|| Error::Protocol("DTX identifier exhausted".into()))?;
                            writer.write_all(&frame(call.channel,next_id,&call.selector,call.args,true)).await?;
                            pending.insert(next_id,call.reply);
                        }
                        received = incoming.recv() => {
                            let msg = received.ok_or_else(|| Error::Protocol("DTX reader stopped".into()))??;
                            let (id, conversation, expects, kind) = metadata(&msg);
                            let _ = events.send(Event::message(label,&msg));
                            if conversation > 0 {
                                if let Some(reply) = pending.remove(&id) {
                                    let result = if kind == 4 { Err(Error::Protocol(format!("{label} DTX error: {:?}",msg.data))) } else { Ok(msg) };
                                    let _ = reply.send(result);
                                }
                            } else {
                                if msg.data.as_ref().and_then(plist::Value::as_string) == Some("_notifyOfPublishedCapabilities:") && let Some(tx) = hello.take() { let _ = tx.send(()); }
                                // Includes daemon-requested reverse channels and observational _XCT_* callbacks.
                                if expects { writer.write_all(&ack(&msg)).await?; }
                            }
                        }
                    }
                }
            }.await;
            if let Err(error) = run {
                state.send_replace(ConnectionState::Disconnected(error.to_string()));
                let _ = events.send(Event::error(label, error.to_string()));
                for (_, reply) in pending {
                    let _ = reply.send(Err(Error::Protocol(error.to_string())));
                }
            }
        });
        let client = Self {
            commands,
            tasks: vec![reader_task, actor],
        };
        tokio::time::timeout(DEADLINE, hello_rx)
            .await
            .map_err(|_| Error::Timeout("DTX capabilities"))?
            .map_err(|_| Error::Protocol("DTX closed during handshake".into()))?;
        Ok(client)
    }
    pub(crate) async fn call(
        &self,
        channel: i32,
        selector: &str,
        args: Vec<AuxValue>,
    ) -> Result<Message> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Call {
                channel,
                selector: selector.into(),
                args,
                reply,
            })
            .await
            .map_err(|_| Error::Protocol("DTX closed".into()))?;
        tokio::time::timeout(DEADLINE, receive)
            .await
            .map_err(|_| Error::Timeout("DTX reply"))?
            .map_err(|_| Error::Protocol("DTX closed before reply".into()))?
    }
    pub(crate) async fn channel(&self, name: &str) -> Result<()> {
        let reply = self
            .call(
                0,
                "_requestChannelWithCode:identifier:",
                vec![AuxValue::U32(1), AuxValue::archived_value(name)],
            )
            .await?;
        if reply.data.is_some() {
            return Err(Error::Protocol(format!(
                "channel rejected: {:?}",
                reply.data
            )));
        }
        Ok(())
    }
}
impl Drop for Dtx {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn frame_roundtrip_and_ack_correlation() {
        let bytes = frame(1, 42, "_XCT_logMessage:", vec![AuxValue::U32(7)], true);
        let msg = Message::from_reader(&mut bytes.as_slice()).await.unwrap();
        assert_eq!(metadata(&msg), (42, 0, true, 2));
        let reply = Message::from_reader(&mut ack(&msg).as_slice())
            .await
            .unwrap();
        assert_eq!(metadata(&reply), (42, 1, false, 0));
        assert_eq!(reply.message_header.channel, 1);
    }
    #[tokio::test]
    async fn actor_correlates_reply_amid_callback_and_detects_disconnect() {
        let (host, mut peer) = tokio::io::duplex(16384);
        let (events, mut observed) = broadcast::channel(32);
        let (state, _) = watch::channel(ConnectionState::Connected);
        let state = Arc::new(state);
        let mut state_rx = state.subscribe();
        let daemon = tokio::spawn(async move {
            let hello = Message::from_reader(&mut peer).await.unwrap();
            assert_eq!(
                hello.data.as_ref().and_then(plist::Value::as_string),
                Some("_notifyOfPublishedCapabilities:")
            );
            peer.write_all(&frame(
                0,
                100,
                "_notifyOfPublishedCapabilities:",
                vec![AuxValue::archived_value(plist::Value::Dictionary(
                    plist::Dictionary::new(),
                ))],
                false,
            ))
            .await
            .unwrap();
            let call = Message::from_reader(&mut peer).await.unwrap();
            peer.write_all(&frame(
                1,
                101,
                "_XCT_logMessage:",
                vec![AuxValue::archived_value("hello")],
                true,
            ))
            .await
            .unwrap();
            let callback_ack = Message::from_reader(&mut peer).await.unwrap();
            assert_eq!(metadata(&callback_ack), (101, 1, false, 0));
            assert_eq!(callback_ack.message_header.channel, 1);
            let mut reply = ack(&call);
            let object = Message::new(
                MessageHeader::new(0, 1, 1, 0, 1, false),
                PayloadHeader::new(),
                None,
                Some("accepted".into()),
            )
            .serialize();
            let data = &object[48..];
            reply[12..16].copy_from_slice(&(16u32 + data.len() as u32).to_le_bytes());
            reply[32] = 3;
            reply[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
            reply.extend_from_slice(data);
            peer.write_all(&reply).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        let client = Dtx::open(Box::new(host), "test", events, state)
            .await
            .unwrap();
        let reply = client.call(1, "example:", vec![]).await.unwrap();
        assert_eq!(
            reply.data.as_ref().and_then(plist::Value::as_string),
            Some("accepted")
        );
        daemon.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), state_rx.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            *state_rx.borrow(),
            ConnectionState::Disconnected(_)
        ));
        let mut callback_seen = false;
        while let Ok(event) = observed.try_recv() {
            callback_seen |= event.detail.contains("_XCT_logMessage:");
        }
        assert!(callback_seen);
    }
}
