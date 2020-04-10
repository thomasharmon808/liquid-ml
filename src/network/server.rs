//! Represents a server node in a distributed system, with implementations
//! provided for `LiquidML` use cases.

use crate::error::LiquidError;
use crate::network;
use crate::network::{Connection, ControlMsg, Message, MessageCodec, Server};
use log::info;
use std::collections::HashMap;
use tokio::io::split;
use tokio::net::TcpListener;
use tokio_util::codec::{FramedRead, FramedWrite};

impl Server {
    /// Create a new `Server` running on the given `address` in the format of
    /// `IP:Port`
    pub async fn new(address: &str) -> Result<Self, LiquidError> {
        Ok(Server {
            msg_id: 0,
            directory: HashMap::new(),
            address: address.to_string(),
        })
    }

    /// A blocking function that allows a `Server` to listen for connections
    /// from newly started `Client`s. When a new `Client` connects to this
    /// `Server`, we add the connection to this `Server.directory`, but do
    /// not listen for further messages from the `Client` since this is not
    /// required for any desired functionality.
    pub async fn accept_new_connections(&mut self) -> Result<(), LiquidError> {
        let mut listener = TcpListener::bind(&self.address).await?;
        loop {
            // wait on connections from new clients
            let (socket, _) = listener.accept().await?;
            let (reader, writer) = split(socket);
            let mut stream = FramedRead::new(reader, MessageCodec::new());
            let sink = FramedWrite::new(writer, MessageCodec::new());
            // Receive the listening IP:Port address of the new client
            let address = network::read_msg(&mut stream).await?;
            let address =
                if let ControlMsg::Introduction { address: a } = address.msg {
                    a
                } else {
                    return Err(LiquidError::UnexpectedMessage);
                };
            // Make the `RegistrationMsg` to send to the new Client to inform
            // them of already existing nodes.
            let target_id = self.directory.len() + 1;
            info!(
                "Connected to address: {:#?}, assigning id: {:#?}",
                address.clone(),
                target_id
            );
            let dir_msg = ControlMsg::Directory {
                dir: self
                    .directory
                    .iter()
                    .map(|(k, v)| (*k, v.address.clone()))
                    .collect(),
            };
            // Add them to our directory after making the `RegistrationMsg`
            // because we don't need to inform them of their own address
            let conn = Connection { address, sink };
            self.directory.insert(target_id, conn);
            // Send the new client the list of existing nodes.
            self.send_msg(target_id, dir_msg).await?;
        }
    }

    // TODO: abstract/merge with Client::send_msg, they are the same
    /// Send the given `message` to a client with the given `target_id`.
    pub async fn send_msg(
        &mut self,
        target_id: usize,
        message: ControlMsg,
    ) -> Result<(), LiquidError> {
        let m = Message {
            sender_id: 0,
            target_id,
            msg_id: self.msg_id,
            msg: message,
        };

        network::send_msg(target_id, m, &mut self.directory).await?;
        self.msg_id += 1;
        Ok(())
    }

    /// Broadcast the given `message` to all currently connected clients
    pub async fn broadcast(
        &mut self,
        message: ControlMsg,
    ) -> Result<(), LiquidError> {
        let d: Vec<usize> = self.directory.iter().map(|(k, _)| *k).collect();
        for k in d {
            self.send_msg(k, message.clone()).await?;
        }
        Ok(())
    }
}
