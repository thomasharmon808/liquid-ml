use crate::error::LiquidError;
use crate::network::message::{ConnectionMsg, RegistrationMsg};
use crate::network::network;
use crate::network::network::Connection;
use serde::Serialize;
use std::collections::HashMap;
use tokio::io::{split, BufReader, BufWriter, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
//TODO: Look at Struct std::net::SocketAddrV4 instead of storing
//      addresses as strings

/// Represents a Client node in a distributed system.
#[derive(Debug)]
pub struct Client {
    /// The `id` of this `Client`
    pub id: usize,
    /// The `address` of this `Client`
    pub address: String,
    /// The id of the current message
    pub msg_id: usize,
    /// A directory which is a map of client id to a [`Connection`](Connection)
    pub directory: HashMap<usize, Connection>,
    /// A buffered connection to the `Server`
    pub server: (
        BufReader<ReadHalf<TcpStream>>,
        BufWriter<WriteHalf<TcpStream>>,
    ),
    /// A `TcpListener` which listens for connections from new `Client`s
    pub listener: TcpListener,
}

/// Methods which allow a `Client` node to start up and connect to a distributed
/// system, listen for new connections from other new `Client`s, send
/// directed communication to other `Client`s, and respond to messages from
/// other `Client`s
#[allow(dead_code)]
impl Client {
    /// Create a new `Client` running on the given `my_addr` IP:Port address,
    /// which connects to a server running on the given `server_addr` IP:Port.
    ///
    /// Constructing the `Client` does these things:
    /// 1. Connects to the server
    /// 2. Sends the server our IP:Port address
    /// 3. Server responds with a `RegistrationMsg`
    /// 4. Connects to all other existing `Client`s which spawns a Tokio task
    ///    for each connection that will read messages from the connection
    ///    and handle it.
    pub async fn new(
        server_addr: String,
        my_addr: String,
    ) -> Result<Self, LiquidError> {
        // Connect to the server
        let server_stream = TcpStream::connect(server_addr).await?;
        let (reader, writer) = split(server_stream);
        let mut read_stream = BufReader::new(reader);
        let mut write_stream = BufWriter::new(writer);
        // Tell the server our addresss
        network::send_msg(&my_addr, &mut write_stream).await?;
        // The Server responds w addresses of all currently connected clients
        let reg =
            network::read_msg::<RegistrationMsg>(&mut read_stream).await?;

        // Initialize ourself
        let mut c = Client {
            id: reg.assigned_id,
            address: my_addr.clone(),
            msg_id: reg.msg_id + 1,
            directory: HashMap::new(),
            server: (read_stream, write_stream),
            listener: TcpListener::bind(my_addr.clone()).await?,
        };

        // Connect to all the clients
        for a in reg.clients {
            c.connect(a).await?;
        }

        Ok(c)
    }

    /// A blocking function that allows a `Client` to listen for connections
    /// from newly started `Client`s. When a new `Client` connects to this
    /// `Client`, we add the connection to this `Client.directory`
    /// and spawn a Tokio task to handle further communication from the new
    /// `Client`
    pub async fn accept_new_connections(&mut self) -> Result<(), LiquidError> {
        loop {
            // wait on connections from new clients
            let (socket, _) = self.listener.accept().await?;
            let (reader, writer) = split(socket);
            let mut buf_reader = BufReader::new(reader);
            let write_stream = BufWriter::new(writer);
            // Read the ConnectionMsg from the new client
            let conn_msg: ConnectionMsg =
                network::read_msg(&mut buf_reader).await?;
            // Add the connection with the new client to this directory
            let conn = Connection {
                address: conn_msg.my_address,
                write_stream,
            };
            // TODO: Close the newly created connections in the error cases
            match self.directory.insert(conn_msg.my_id, conn) {
                Some(_) => return Err(LiquidError::ReconnectionError),
                None => {
                    // spawn a tokio task to handle new messages from the client
                    // that we just connected to
                    // TODO: change the callback given to self.recv_msg
                    self.recv_msg(buf_reader, |x| println!("{:#?}", x));
                    self.increment_msg_id(conn_msg.msg_id);
                }
            };
        }
    }

    /// Connect to a running `Client` with the given `(id, IP:Port)` information.
    /// After connecting, add the `Connection` to the other `Client` to this
    /// `Client.directory` for sending later messages to the `Client`. Finally,
    /// spawn a Tokio task to read further messages from the `Client` and
    /// handle the message.
    pub(crate) async fn connect(
        &mut self,
        client: (usize, String),
    ) -> Result<(), LiquidError> {
        // Connect to the given client
        let stream = TcpStream::connect(client.1.clone()).await?;
        let (reader, writer) = split(stream);
        let read_stream = BufReader::new(reader);
        let write_stream = BufWriter::new(writer);

        // Make the connection struct which holds the stream for sending msgs
        let conn = Connection {
            address: client.1.clone(),
            write_stream,
        };

        // Add the connection to our directory of connections to other clients
        match self.directory.insert(client.0, conn) {
            Some(_) => Err(LiquidError::ReconnectionError),
            None => {
                // spawn a tokio task to handle new messages from the client
                // that we just connected to
                // TODO: change the callback given to self.recv_msg
                self.recv_msg(read_stream, |x| println!("{:?}", x));
                // send the client our id and address so they can add us to
                // their directory
                let conn_msg = ConnectionMsg {
                    my_id: self.id,
                    msg_id: self.msg_id,
                    my_address: self.address.clone(),
                };
                self.send_msg(client.0, &conn_msg).await?;

                println!("Id: {:#?} at address: {:#?} connected to id: {:#?} at address: {:#?}", self.id, self.address, client.0, client.1);
                self.send_msg(client.0, &"Hi".to_string()).await?;

                Ok(())
            }
        }
    }

    /// Send the given `message` to a client with the given `target_id`.
    pub async fn send_msg<T: Serialize>(
        &mut self,
        target_id: usize,
        message: &T,
    ) -> Result<(), LiquidError> {
        match self.directory.get_mut(&target_id) {
            None => Err(LiquidError::UnknownId),
            Some(conn) => {
                network::send_msg(message, &mut conn.write_stream).await?;
                self.msg_id += 1;
                Ok(())
            }
        }
    }

    /// Spawns a Tokio task to read messages from the given `reader` and
    /// handle responding to them.
    pub(crate) fn recv_msg(
        &mut self,
        mut reader: BufReader<ReadHalf<TcpStream>>,
        callback: fn(String) -> (), // TODO: fix signature
    ) {
        // NOTE: may need to do tokio::runtime::Runtime::spawn or
        // tokio::runtime::Handle::spawn in order to actually place spawned
        // task into an executor
        tokio::spawn(async move {
            loop {
                let s: String = network::read_msg(&mut reader).await.unwrap();
                callback(s);
            }
        });
    }

    fn increment_msg_id(&mut self, id: usize) {
        self.id = std::cmp::max(self.id, id) + 1;
    }
}
