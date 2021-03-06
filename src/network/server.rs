//! Represents a server node in a distributed system, with implementations
//! provided for `LiquidML` use cases.
use crate::error::LiquidError;
use crate::network::{message, Connection, ControlMsg, Message, MessageCodec};
use log::info;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::io::split;
use tokio::net::TcpListener;
use tokio_util::codec::{FramedRead, FramedWrite};

/// Represents a registration `Server` in a distributed system.
#[derive(Debug)]
pub struct Server {
    /// The `address` of this `Server`
    pub(crate) address: SocketAddr,
    /// The id of the current message
    pub(crate) msg_id: usize,
    /// A directory which is a `HashMap` of network names to that network,
    /// (a `HashMap` of `node_id` to a [`Connection`]).
    ///
    /// [`Connection`]: struct.Connection.html
    pub(crate) directory:
        HashMap<String, HashMap<usize, Connection<ControlMsg>>>,
}

impl Server {
    /// Create a new `Server` running on the given `address` in the format of
    /// `IP:Port`.
    pub async fn new(address: &str) -> Result<Self, LiquidError> {
        Ok(Server {
            msg_id: 0,
            directory: HashMap::new(),
            address: address.parse().unwrap(),
        })
    }

    /// A blocking function that allows a `Server` to listen for connections
    /// from newly started [`Client`]s. When a new [`Client`] connects to this
    /// `Server`, we add the connection to our directory for sending
    /// `ControlMsg::Kill` messages, but do not listen for further messages
    /// from the [`Client`] since this is not required for performing simple
    /// registration.
    ///
    /// [`Client`]: struct.Client.html
    pub async fn accept_new_connections(&mut self) -> Result<(), LiquidError> {
        let mut listener = TcpListener::bind(&self.address).await?;
        loop {
            // wait on connections from new clients
            let (socket, _) = listener.accept().await?;
            let (reader, writer) = split(socket);
            let mut stream = FramedRead::new(reader, MessageCodec::new());
            let sink = FramedWrite::new(writer, MessageCodec::new());
            // Receive the listening IP:Port address of the new client
            let address = message::read_msg(&mut stream).await?;
            let (address, network_name) = if let ControlMsg::Introduction {
                address,
                network_name,
            } = address.msg
            {
                (address, network_name)
            } else {
                return Err(LiquidError::UnexpectedMessage);
            };
            let conn = Connection { address, sink };

            let target_id;
            let dir;
            match self.directory.get_mut(&network_name) {
                Some(d) => {
                    // there are some existing clients of this type
                    target_id = d.len() + 1; // node id's start at 1
                    dir = d.iter().map(|(k, v)| (*k, v.address)).collect();
                    d.insert(target_id, conn);
                }
                None => {
                    target_id = 1;
                    dir = Vec::new();
                    let mut d = HashMap::new();
                    d.insert(target_id, conn);
                    self.directory.insert(network_name.clone(), d);
                }
            };

            info!(
                "Connected to address: {:#?} joining network {:#?}, assigning id: {:#?}",
                &address,
                &network_name,
                target_id
            );

            // Send the new client the list of existing nodes.
            let dir_msg = ControlMsg::Directory { dir };
            self.send_msg(target_id, &network_name, dir_msg).await?;
        }
    }

    /// Send the given `message` to a [`Client`] running in the network with
    /// the given `network_name` and with the given `target_id`.
    ///
    /// [`Client`]: struct.Client.html
    pub async fn send_msg(
        &mut self,
        target_id: usize,
        network_name: &str,
        message: ControlMsg,
    ) -> Result<(), LiquidError> {
        let m = Message::new(self.msg_id, 0, target_id, message);
        message::send_msg(
            target_id,
            m,
            self.directory.get_mut(network_name).unwrap(),
        )
        .await?;
        self.msg_id += 1;
        Ok(())
    }

    /// Broadcast the given `message` to all currently connected [`Clients`]
    /// in the network with the given `network_name`
    ///
    /// [`Client`]: struct.Client.html
    pub async fn broadcast(
        &mut self,
        message: ControlMsg,
        network_name: &str,
    ) -> Result<(), LiquidError> {
        let d: Vec<usize> = self
            .directory
            .iter()
            .find(|(k, _)| **k == network_name)
            .unwrap()
            .1
            .iter()
            .map(|(k, _)| *k)
            .collect();
        for k in d {
            self.send_msg(k, network_name, message.clone()).await?;
        }
        Ok(())
    }
}
