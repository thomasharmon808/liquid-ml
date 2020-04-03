//! This module defines an application the highest level component of a liquid_ml system. The
//! application exposes a KVStore and a blob receiver that can be used to send random blocs across
//! the network. The blob receiver is designed to be used for control messages.
//!
//! A user of the liquid_ml system need only instantiate an application and provide it an async
//! function to be run. The application grants access to its node_id so different tasks can be done
//! on different nodes.
//!
//! Detailed examples that use the application can be found in the examples directory of this
//! crate.

use crate::dataframe::{DataFrame, Rower};
use crate::error::LiquidError;
use crate::kv::{KVStore, Key, Value};
use bincode::{deserialize, serialize};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, File};
use std::future::Future;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::Arc;
use tokio::sync::{mpsc, mpsc::Receiver, Notify};

/// Represents an application
pub struct Application {
    /// A pointer to the KVStore that stores all the data for the application
    pub kv: Arc<KVStore<DataFrame>>,
    /// The id of this node, assigned by the registration server
    pub node_id: usize,
    /// A receiver for blob messages that can b processed by the user
    pub blob_receiver: Receiver<Value>,
    /// The number of nodes in this network
    /// NOTE: Panics if `num_nodes` is inconsistent with this network
    num_nodes: usize,
    /// A notifier that gets notified when the server has sent a kill message
    pub kill_notifier: Arc<Notify>,
}

impl Application {
    /// Create a new `liquid_ml` application that runs at `my_addr` and will
    /// wait to connect to `num_nodes` nodes after registering with the
    /// `Server` at the `server_addr` before returning.
    pub async fn new(
        my_addr: &str,
        server_addr: &str,
        num_nodes: usize,
    ) -> Result<Self, LiquidError> {
        let (blob_sender, blob_receiver) = mpsc::channel(2);
        let kill_notifier = Arc::new(Notify::new());
        let kv = KVStore::new(
            server_addr,
            my_addr,
            blob_sender,
            kill_notifier.clone(),
            num_nodes,
            true,
        )
        .await;
        let node_id = kv.id;
        Ok(Application {
            kv,
            node_id,
            blob_receiver,
            num_nodes,
            kill_notifier,
        })
    }

    /// Create a new application and split the given SoR file across all the
    /// nodes in the network. Assigns a key with the name `df_name` to
    /// the `DataFrame` chunk for this node.
    ///
    /// Note: assumes the entire SoR file is present on all nodes
    pub async fn from_sor(
        file_name: &str,
        my_addr: &str,
        server_addr: &str,
        num_nodes: usize,
        df_name: &str,
    ) -> Result<Self, LiquidError> {
        let app = Application::new(my_addr, server_addr, num_nodes).await?;
        let file = fs::metadata(file_name).unwrap();
        let f: File = File::open(file_name).unwrap();
        let mut reader = BufReader::new(f);
        let mut size = file.len() / num_nodes as u64;
        // Note: Node ids start at 1
        let from = size * (app.node_id - 1) as u64;

        // advance the reader to this node's starting index then
        // find the next newline character
        let mut buffer = Vec::new();
        reader.seek(SeekFrom::Start(from + size)).unwrap();
        reader.read_until(b'\n', &mut buffer).unwrap();
        size += buffer.len() as u64 + 1;

        let df = DataFrame::from_sor(file_name, from as usize, size as usize);
        let key = Key::new(df_name, app.node_id);
        app.kv.put(&key, df).await?;
        Ok(app)
    }

    /// Perform a distributed map operation on the `DataFrame` associated with
    /// the `df_name` with the given `rower`. Returns `Some(rower)` (of the
    /// joined results) if the `node_id` of this `Application` is `1`, and
    /// `None` otherwise.
    ///
    /// A local `pmap` is used on each node to map over that nodes' chunk.
    /// By default, each node will use the number of threads available on that
    /// machine.
    ///
    /// NOTE:
    /// There is an important design decision that comes with a distinct trade
    /// off here. The trade off is:
    /// 1. Join the last node with the next one until you get to the end. This
    ///    has reduced memory requirements but a performance impact because
    ///    of the synchronous network calls
    /// 2. Join all nodes with one node by sending network messages
    ///    concurrently to the final node. This has increased memory
    ///    requirements and greater complexity but greater performance because
    ///    all nodes can asynchronously send to one node at the same time.
    ///
    /// This implementation went with option 1 for simplicity reasons
    pub async fn pmap<R>(
        &mut self,
        df_name: &str,
        rower: R,
    ) -> Result<Option<R>, LiquidError>
    where
        R: Rower + Serialize + DeserializeOwned + Send + Clone,
    {
        match self.kv.get(&Key::new(df_name, self.node_id)).await {
            Ok(df) => {
                let mut res = df.pmap(rower);
                if self.node_id == self.num_nodes {
                    // we are the last node
                    let blob = serialize(&res)?;
                    self.kv.send_blob(self.node_id - 1, blob).await?;
                    Ok(None)
                } else {
                    let mut blob = self.blob_receiver.recv().await.unwrap();
                    let external_rower: R = deserialize(&blob[..])?;
                    res = res.join(external_rower);
                    if self.node_id != 1 {
                        blob = serialize(&res)?;
                        self.kv.send_blob(self.node_id - 1, blob).await?;
                        Ok(None)
                    } else {
                        Ok(Some(res))
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Given a function run it on this application. This function only terminates when a kill
    /// signal from the server has been sent. `examples/demo_client.rs` is a good starting point to
    /// see this in action
    pub async fn run<F, Fut>(self, f: F)
    where
        Fut: Future<Output = ()>,
        F: FnOnce(Arc<KVStore<DataFrame>>) -> Fut,
    {
        f(self.kv.clone()).await;
        self.kill_notifier.notified().await;
    }
}
