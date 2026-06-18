use futures::StreamExt;
use samod::{ConnectionInfo, Repo};
use tokio::{select, sync::watch};
use tokio_util::sync::CancellationToken;

use crate::helpers::spawn_utils::spawn_named;

#[derive(Debug)]
pub struct PeerWatcher {
    server_info_tx: watch::Sender<Option<ConnectionInfo>>,
    token: CancellationToken,
}

impl Drop for PeerWatcher {
    fn drop(&mut self) {
        self.token.cancel()
    }
}

impl PeerWatcher {
    pub fn new(repo_handle: Repo) -> Self {
        let (tx, _rx) = watch::channel(None);
        let tx_clone = tx.clone();
        let repo_handle_clone = repo_handle.clone();
        let token = CancellationToken::new();
        let token_clone = token.clone();
        spawn_named("Peer watcher", async move {
            let (_, stream) = repo_handle_clone.connected_peers();
            tokio::pin!(stream);
            loop {
                select! {
                    _ = token_clone.cancelled() => { break; }
                    Some(peers) = stream.next() => {
                        // Currently, we only ever have 1 peer: the server.
                        // Therefore, this code expects that the server is the first and only peer, if it's connected.
                        // When we move to more peers, we'll need to figure out a way to identify the server here.
                        let info = peers.into_iter().next();
                        _ = tx_clone.send_if_modified(|old_info| {
                            // this clone probably sucks, maybe fix this
                            let old = old_info.clone();
                            let new_info = Self::update_server_info(old, info);
                            let changed = new_info != *old_info;
                            *old_info = new_info;
                            changed
                        });
                    }
                }
            }
        });

        Self {
            server_info_tx: tx,
            token,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<ConnectionInfo>> {
        self.server_info_tx.subscribe()
    }

    pub fn get_server_info(&self) -> Option<ConnectionInfo> {
        return self.server_info_tx.subscribe().borrow().clone();
    }

    fn update_server_info(
        old_info: Option<ConnectionInfo>,
        new_info: Option<ConnectionInfo>,
    ) -> Option<ConnectionInfo> {
        let Some(new_info) = new_info else {
            return None;
        };
        if old_info.is_none() {
            return Some(new_info);
        }
        let mut info = old_info.unwrap();
        info.last_received = new_info.last_received;
        info.last_sent = new_info.last_sent;

        for (doc_id, new_doc_state) in &new_info.docs {
            if let Some(old_doc_state) = info.docs.get(doc_id) {
                // If we got beheaded, skip this doc.
                if new_doc_state
                    .last_acked_heads
                    .as_ref()
                    .is_some_and(|h| h.is_empty())
                    && old_doc_state
                        .last_acked_heads
                        .as_ref()
                        .is_some_and(|h| !h.is_empty())
                {
                    continue;
                }
            }
            info.docs.insert(doc_id.clone(), new_doc_state.clone());
        }
        Some(info)
    }
}
