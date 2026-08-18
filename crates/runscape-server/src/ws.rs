//! `WS /ws/v1` (task T3.4).
//!
//! One snapshot on connect, incremental frames afterwards. The origin
//! allow-list is applied by the router's middleware before the upgrade ever
//! reaches this module.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::frames::ClientFrame;
use crate::state::AppState;

pub async fn upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| serve(socket, state))
}

async fn serve(socket: WebSocket, state: AppState) {
    // Subscribe before sending the snapshot: a service that starts while the
    // snapshot is being built must still reach this client.
    let mut updates = state.subscribe();
    let (mut sink, mut stream) = socket.split();

    if !send_snapshot(&mut sink, &state).await {
        return;
    }

    loop {
        tokio::select! {
            frame = updates.recv() => match frame {
                Ok(frame) => {
                    if sink.send(Message::Text(frame.as_ref().into())).await.is_err() {
                        return;
                    }
                }
                // A client that cannot keep up is dropped rather than buffered
                // without bound; it reconnects and gets a fresh snapshot
                // (`docs/api-contract.md`).
                Err(RecvError::Lagged(missed)) => {
                    warn!(missed, "dropping a slow websocket client");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
                Err(RecvError::Closed) => return,
            },
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientFrame>(&text) {
                        // The only request a client may make.
                        Ok(ClientFrame::Resnapshot) => {
                            if !send_snapshot(&mut sink, &state).await {
                                return;
                            }
                        }
                        Err(_) => debug!("ignoring unrecognised client frame"),
                    }
                }
                Some(Ok(Message::Close(_))) | None => return,
                // Ping/pong are handled by axum; binary frames are not part of
                // the contract and are ignored rather than answered.
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    debug!(%error, "websocket read failed");
                    return;
                }
            },
        }
    }
}

/// Returns false when the socket is gone.
async fn send_snapshot(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: &AppState,
) -> bool {
    let frame = state.snapshot_frame().await;
    let Some(json) = state.encode(&frame) else {
        return false;
    };
    sink.send(Message::Text(json.as_ref().into())).await.is_ok()
}
