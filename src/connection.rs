use crate::log::{Log, Message};
use chrono::{DateTime, Utc};
use std::{
    net::IpAddr,
    sync::mpsc::{channel, Receiver, Sender},
    time::SystemTime,
};

/// Export Connection Flags
pub struct Flags;

/// `Connection` Flags
impl Flags {
    /// Default, no flags raised.
    pub const UNKNOWN: u8 = 0x00;
    /// Seen pipelined requests.
    pub const PIPELINED: u8 = 0x01;
    /// Seen extra data after a HTTP 0.9 communication.
    pub const HTTP_0_9_EXTRA: u8 = 0x02;
}

/// Stores information about the session.
pub struct Connection {
    /// Client IP address.
    pub client_addr: Option<IpAddr>,
    /// Client port.
    pub client_port: Option<u16>,
    /// Server IP address.
    pub server_addr: Option<IpAddr>,
    /// Server port.
    pub server_port: Option<u16>,

    /// Messages channel associated with this connection.
    log_channel: (Sender<Message>, Receiver<Message>),

    /// Parsing flags.
    pub flags: u8,
    /// When was this connection opened?
    pub open_timestamp: DateTime<Utc>,
    /// When was this connection closed?
    pub close_timestamp: DateTime<Utc>,
    /// Inbound data counter.
    pub in_data_counter: i64,
    /// Outbound data counter.
    pub out_data_counter: i64,
}

impl Default for Connection {
    /// Returns a new Connection instance with default values.
    fn default() -> Self {
        Self {
            client_addr: None,
            client_port: None,
            server_addr: None,
            server_port: None,
            log_channel: channel(),
            flags: 0,
            open_timestamp: DateTime::<Utc>::from(SystemTime::now()),
            close_timestamp: DateTime::<Utc>::from(SystemTime::now()),
            in_data_counter: 0,
            out_data_counter: 0,
        }
    }
}

impl Connection {
    /// Opens a connection. This function will essentially only store the provided data
    /// for future reference.
    pub fn open(
        &mut self,
        client_addr: Option<IpAddr>,
        client_port: Option<u16>,
        server_addr: Option<IpAddr>,
        server_port: Option<u16>,
        timestamp: Option<DateTime<Utc>>,
    ) {
        self.client_addr = client_addr;
        self.client_port = client_port;
        self.server_addr = server_addr;
        self.server_port = server_port;

        // Remember when the connection was opened.
        if let Some(timestamp) = timestamp {
            self.open_timestamp = timestamp;
        }
    }

    /// Closes the connection.
    pub fn close(&mut self, timestamp: Option<DateTime<Utc>>) {
        // Update timestamp.
        if let Some(timestamp) = timestamp {
            self.close_timestamp = timestamp;
        }
    }

    /// Keeps track of inbound packets and data.
    pub fn track_inbound_data(&mut self, len: usize) {
        self.in_data_counter = (self.in_data_counter as u64).wrapping_add(len as u64) as i64;
    }

    /// Keeps track of outbound packets and data.
    pub fn track_outbound_data(&mut self, len: usize) {
        self.out_data_counter = (self.out_data_counter as u64).wrapping_add(len as u64) as i64;
    }

    /// Return the log channel sender
    pub fn get_sender(&self) -> &Sender<Message> {
        &self.log_channel.0
    }

    /// Drains and returns a vector of all current logs received by the log channel
    pub fn get_logs(&self) -> Vec<Log> {
        let mut logs = Vec::with_capacity(8);
        while let Ok(message) = self.log_channel.1.try_recv() {
            logs.push(Log::new(self, message))
        }
        logs
    }

    /// Returns the next logged message received by the log channel
    pub fn get_next_log(&self) -> Option<Log> {
        self.log_channel
            .1
            .try_recv()
            .map(|message| Log::new(self, message))
            .ok()
    }
}

impl PartialEq for Connection {
    /// Returns true if connections are the same, false otherwise.
    fn eq(&self, rhs: &Self) -> bool {
        self.client_addr == rhs.client_addr
            && self.client_port == rhs.client_port
            && self.server_addr == rhs.server_addr
            && self.server_port == rhs.server_port
    }
}
