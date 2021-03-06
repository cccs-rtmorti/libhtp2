#![allow(non_snake_case)]
use chrono::{DateTime, Utc};
use htp::{
    bstr::Bstr,
    config::{Config, HtpServerPersonality},
    connection::Flags as ConnectionFlags,
    connection_parser::{ConnectionParser, HtpStreamState},
    error::Result,
    log::{HtpLogCode, HtpLogLevel},
    transaction::{
        Data, HtpAuthType, HtpDataSource, HtpProtocol, HtpRequestProgress, HtpResponseNumber,
        HtpResponseProgress,
    },
    util::{FlagOperations, HtpFileSource, HtpFlags},
};
use std::{
    convert::TryInto,
    env,
    iter::IntoIterator,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    slice,
    time::SystemTime,
};

// import common testing utilities
mod common;
#[derive(Debug)]
enum Chunk {
    Client(Vec<u8>),
    Server(Vec<u8>),
}

struct MainUserData {
    pub request_data: Vec<Bstr>,
    pub response_data: Vec<Bstr>,
}

impl MainUserData {
    pub fn new() -> Self {
        Self {
            request_data: Vec::with_capacity(5),
            response_data: Vec::with_capacity(5),
        }
    }
}

#[derive(Debug)]
struct TestInput {
    chunks: Vec<Chunk>,
}

impl IntoIterator for TestInput {
    type Item = Chunk;
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.chunks.into_iter()
    }
}

impl TestInput {
    fn new(file: PathBuf) -> Self {
        let input = std::fs::read(file);
        assert!(input.is_ok());
        let input = input.unwrap();

        let mut test_input = TestInput { chunks: Vec::new() };
        let mut current = Vec::<u8>::new();
        let mut client = true;
        for line in input.split(|c| *c == b'\n') {
            if line.len() >= 3
                && ((line[0] == b'>' && line[1] == b'>' && line[2] == b'>')
                    || (line[0] == b'<' && line[1] == b'<' && line[2] == b'<'))
            {
                if !current.is_empty() {
                    // Pop off the CRLF from the last line, which
                    // just separates the previous data from the
                    // boundary <<< >>> chars and isn't actual data
                    if let Some(b'\n') = current.last() {
                        current.pop();
                    }
                    if let Some(b'\r') = current.last() {
                        current.pop();
                    }
                    test_input.append(client, current);
                    current = Vec::<u8>::new();
                }
                client = line[0] == b'>';
            } else {
                current.append(&mut line.to_vec());
                current.push(b'\n');
            }
        }
        // Remove the '\n' we would have appended for EOF
        current.pop();
        test_input.append(client, current);
        test_input
    }

    fn append(&mut self, client: bool, data: Vec<u8>) {
        if client {
            self.chunks.push(Chunk::Client(data));
        } else {
            self.chunks.push(Chunk::Server(data));
        }
    }
}

#[derive(Debug)]
enum TestError {
    //MultipleClientChunks,
    //MultipleServerChunks,
    StreamError,
}

struct Test {
    connp: ConnectionParser,
    basedir: PathBuf,
}

fn TestConfig() -> Config {
    let mut cfg = Config::default();
    cfg.set_server_personality(HtpServerPersonality::APACHE_2)
        .unwrap();
    // The default bomb limit may be slow in some development environments causing tests to fail.
    cfg.compression_options.set_time_limit(std::u32::MAX);
    cfg.set_parse_urlencoded(true);
    cfg.set_parse_multipart(true);

    return cfg;
}

impl Test {
    fn new(cfg: Config) -> Self {
        let basedir = if let Ok(dir) = std::env::var("srcdir") {
            PathBuf::from(dir)
        } else {
            let mut base = PathBuf::from(
                env::var("CARGO_MANIFEST_DIR").expect("Could not determine test file directory"),
            );
            base.push("tests");
            base.push("files");
            base
        };

        let connp = ConnectionParser::new(cfg);
        Test { connp, basedir }
    }
    fn new_with_callbacks() -> Self {
        let mut cfg = TestConfig();
        cfg.register_response_body_data(response_body_data);
        cfg.register_request_body_data(request_body_data);
        let mut t = Test::new(cfg);
        // Configure user data and callbacks
        t.connp
            .response_mut()
            .set_user_data(Box::new(MainUserData::new()));
        t
    }
    fn run(&mut self, file: &str) -> std::result::Result<(), TestError> {
        let tv_start = DateTime::<Utc>::from(SystemTime::now());
        self.connp.open(
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            Some(10000),
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            Some(80),
            Some(tv_start),
        );

        let mut path = self.basedir.clone();
        path.push(file);
        let test = TestInput::new(path);
        let mut request_buf: Option<Vec<u8>> = None;
        let mut response_buf: Option<Vec<u8>> = None;
        for chunk in test {
            match chunk {
                Chunk::Client(data) => {
                    let rc = self
                        .connp
                        .request_data(data.as_slice().into(), Some(tv_start));

                    if rc == HtpStreamState::ERROR {
                        return Err(TestError::StreamError);
                    }

                    if rc == HtpStreamState::DATA_OTHER {
                        let consumed = self
                            .connp
                            .request_data_consumed()
                            .try_into()
                            .expect("Error retrieving number of consumed bytes.");
                        let mut remaining = Vec::with_capacity(data.len() - consumed);
                        remaining.extend_from_slice(&data[consumed..]);
                        request_buf = Some(remaining);
                    }
                }
                Chunk::Server(data) => {
                    // If we have leftover data from before then use it first
                    if let Some(ref response_remaining) = response_buf {
                        let rc = (&mut self.connp)
                            .response_data(response_remaining.into(), Some(tv_start));
                        response_buf = None;
                        if rc == HtpStreamState::ERROR {
                            return Err(TestError::StreamError);
                        }
                    }

                    // Now use up this data chunk
                    let rc =
                        (&mut self.connp).response_data(data.as_slice().into(), Some(tv_start));
                    if rc == HtpStreamState::ERROR {
                        return Err(TestError::StreamError);
                    }

                    if rc == HtpStreamState::DATA_OTHER {
                        let consumed = self
                            .connp
                            .response_data_consumed()
                            .try_into()
                            .expect("Error retrieving number of consumed bytes.");
                        let mut remaining = Vec::with_capacity(data.len() - consumed);
                        remaining.extend_from_slice(&data[consumed..]);
                        response_buf = Some(remaining);
                    }

                    // And check if we also had some input data buffered
                    if let Some(ref request_remaining) = request_buf {
                        let rc = self
                            .connp
                            .request_data(request_remaining.into(), Some(tv_start));
                        request_buf = None;
                        if rc == HtpStreamState::ERROR {
                            return Err(TestError::StreamError);
                        }
                    }
                }
            }
        }

        // Clean up any remaining server data
        if let Some(ref response_remaining) = response_buf {
            let rc = (&mut self.connp).response_data(response_remaining.into(), Some(tv_start));
            if rc == HtpStreamState::ERROR {
                return Err(TestError::StreamError);
            }
        }
        self.connp
            .close(Some(DateTime::<Utc>::from(SystemTime::now())));
        Ok(())
    }
}

fn response_body_data(d: &mut Data) -> Result<()> {
    let user_data = unsafe { (*d.tx()).user_data_mut::<MainUserData>().unwrap() };
    user_data
        .response_data
        .push(Bstr::from(d.as_slice().unwrap()));
    Ok(())
}

fn request_body_data(d: &mut Data) -> Result<()> {
    let user_data = unsafe { (*d.tx()).user_data_mut::<MainUserData>().unwrap() };
    user_data
        .request_data
        .push(Bstr::from(d.as_slice().unwrap()));
    Ok(())
}

#[test]
fn AdHoc() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("00-adhoc.t").is_ok());
}

#[test]
fn Get() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("01-get.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert!(tx.request_uri.as_ref().unwrap().eq("/?p=%20"));

    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .eq("p=%20"));

    assert_contains_param!(&tx.request_params, "p", " ");
}

#[test]
fn GetEncodedRelPath() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("99-get.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert!(tx.request_hostname.as_ref().unwrap().eq("www.example.com"));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/images.gif"));
}

#[test]
fn ApacheHeaderParsing() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("02-header-test-apache2.t").is_ok());

    let tx = t.connp.tx(0).expect("expected tx to exist");

    let actual: Vec<(&[u8], &[u8])> = (&tx.request_headers)
        .into_iter()
        .map(|(_, val)| (val.name.as_slice(), val.value.as_slice()))
        .collect();

    let expected: Vec<(&[u8], &[u8])> = [
        ("Invalid-Folding", "1"),
        ("Valid-Folding", "2 2"),
        ("Normal-Header", "3"),
        ("Invalid Header Name", "4"),
        ("Same-Name-Headers", "5, 6"),
        ("Empty-Value-Header", ""),
        ("", "8, "),
        ("Header-With-LWS-After", "9"),
        ("Header-With-NUL", "BEFORE"),
    ]
    .iter()
    .map(|(key, val)| (key.as_bytes(), val.as_bytes()))
    .collect();
    assert_eq!(
        actual,
        expected,
        "{:?} != {:?}",
        actual
            .clone()
            .into_iter()
            .map(|(key, val)| (
                String::from_utf8_lossy(key).to_string(),
                String::from_utf8_lossy(val).to_string()
            ))
            .collect::<Vec<(String, String)>>(),
        expected
            .clone()
            .into_iter()
            .map(|(key, val)| (
                String::from_utf8_lossy(key).to_string(),
                String::from_utf8_lossy(val).to_string()
            ))
            .collect::<Vec<(String, String)>>(),
    );
}

#[test]
fn PostUrlencoded() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("03-post-urlencoded.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    // Transaction 1
    let tx = t.connp.tx(0).unwrap();

    assert_contains_param!(&tx.request_params, "p", "0123456789");

    assert_eq!(tx.request_progress, HtpRequestProgress::COMPLETE);
    assert_eq!(tx.response_progress, HtpResponseProgress::COMPLETE);

    assert_response_header_eq!(tx, "Server", "Apache");

    // Transaction 2
    let tx2 = t.connp.tx(1).unwrap();

    assert_eq!(tx2.request_progress, HtpRequestProgress::COMPLETE);
    assert_eq!(tx2.response_progress, HtpResponseProgress::COMPLETE);

    assert_response_header_eq!(tx2, "Server", "Apache");
}

#[test]
fn PostUrlencodedChunked() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("04-post-urlencoded-chunked.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_contains_param!(&tx.request_params, "p", "0123456789");
    assert_eq!(25, tx.request_message_len);
    assert_eq!(12, tx.request_entity_len);
}

#[test]
fn Expect() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("05-expect.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    // The interim header from the 100 response should not be among the final headers.
    assert!(tx.request_headers.get_nocase_nozero("Header1").is_none());
}

#[test]
fn UriNormal() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("06-uri-normal.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let _tx = t.connp.tx(0).unwrap();
}

#[test]
fn PipelinedConn() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("07-pipelined-connection.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    assert!(t.connp.conn.flags.is_set(ConnectionFlags::PIPELINED));

    let _tx = t.connp.tx(0).unwrap();
}

#[test]
fn NotPipelinedConn() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("08-not-pipelined-connection.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    assert!(!t.connp.conn.flags.is_set(ConnectionFlags::PIPELINED));

    let tx = t.connp.tx(0).unwrap();

    assert!(!tx.flags.is_set(HtpFlags::MULTI_PACKET_HEAD));
}

#[test]
fn MultiPacketRequest() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("09-multi-packet-request-head.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::MULTI_PACKET_HEAD));
}

#[test]
fn HeaderHostParsing() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("10-host-in-headers.t").is_ok());
    assert_eq!(4, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert!(tx1.request_hostname.as_ref().unwrap().eq("www.example.com"));

    let tx2 = t.connp.tx(1).unwrap();

    assert!(tx2
        .request_hostname
        .as_ref()
        .unwrap()
        .eq("www.example.com."));

    let tx3 = t.connp.tx(2).unwrap();

    assert!(tx3.request_hostname.as_ref().unwrap().eq("www.example.com"));

    let tx4 = t.connp.tx(3).unwrap();

    assert!(tx4.request_hostname.as_ref().unwrap().eq("www.example.com"));
}

#[test]
fn ResponseWithoutContentLength() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("11-response-stream-closure.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());
}

#[test]
fn FailedConnectRequest() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("12-connect-request.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());
    assert!(tx.request_method.as_ref().unwrap().eq("CONNECT"));
    assert!(tx.response_content_type.as_ref().unwrap().eq("text/html"));
    assert!(tx
        .response_message
        .as_ref()
        .unwrap()
        .eq("Method Not Allowed"));
    assert!(tx.response_status_number.eq_num(405));
}

#[test]
fn CompressedResponseContentType() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("13-compressed-response-gzip-ct.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert_eq!(187, tx.response_message_len);
    assert_eq!(225, tx.response_entity_len);
    assert!(tx
        .response_message
        .as_ref()
        .unwrap()
        .eq("Moved Temporarily"));
}

#[test]
fn CompressedResponseChunked() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("14-compressed-response-gzip-chunked.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(28261, tx.response_message_len);

    assert_eq!(159_590, tx.response_entity_len);
}

#[test]
fn SuccessfulConnectRequest() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("15-connect-complete.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    // TODO: Update the test_run() function to provide better
    //       simulation of real traffic. At the moment, it does not
    //       invoke inbound parsing after outbound parsing returns
    //       HTP_DATA_OTHER, which is why the check below fails.
    //assert!(tx.is_complete());

    assert!(tx.request_method.as_ref().unwrap().eq("CONNECT"));

    assert!(tx.response_status_number.eq_num(200));
}

#[test]
fn ConnectRequestWithExtraData() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("16-connect-extra.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert!(tx1.is_complete());
    assert!(tx1.response_content_type.as_ref().unwrap().eq("text/html"));

    let tx2 = t.connp.tx(1).unwrap();

    assert!(tx2.is_complete());
}

#[test]
fn Multipart() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("17-multipart-1.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_contains_param!(&tx.request_params, "field1", "0123456789");
    assert_contains_param!(&tx.request_params, "field2", "9876543210");
}

#[test]
fn CompressedResponseDeflate() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("18-compressed-response-deflate.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(755, tx.response_message_len);

    assert_eq!(1433, tx.response_entity_len);
}

#[test]
fn UrlEncoded() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("19-urlencoded-test.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert!(tx.request_method.as_ref().unwrap().eq("POST"));
    assert!(tx.request_uri.as_ref().unwrap().eq("/?p=1&q=2"));
    assert_contains_param_source!(&tx.request_params, HtpDataSource::BODY, "p", "3");
    assert_contains_param_source!(&tx.request_params, HtpDataSource::BODY, "q", "4");
    assert_contains_param_source!(&tx.request_params, HtpDataSource::BODY, "z", "5");
}

#[test]
fn AmbiguousHost() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("20-ambiguous-host.t").is_ok());

    assert_eq!(5, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert!(tx1.is_complete());
    assert!(!tx1.flags.is_set(HtpFlags::HOST_AMBIGUOUS));

    let tx2 = t.connp.tx(1).unwrap();

    assert!(tx2.is_complete());
    assert!(tx2.flags.is_set(HtpFlags::HOST_AMBIGUOUS));
    assert!(tx2.request_hostname.as_ref().unwrap().eq("example.com"));

    let tx3 = t.connp.tx(2).unwrap();

    assert!(tx3.is_complete());
    assert!(!tx3.flags.is_set(HtpFlags::HOST_AMBIGUOUS));
    assert!(tx3.request_hostname.as_ref().unwrap().eq("www.example.com"));
    assert_eq!(Some(8001), tx3.request_port_number);

    let tx4 = t.connp.tx(3).unwrap();

    assert!(tx4.is_complete());
    assert!(tx4.flags.is_set(HtpFlags::HOST_AMBIGUOUS));
    assert!(tx4.request_hostname.as_ref().unwrap().eq("www.example.com"));
    assert_eq!(Some(8002), tx4.request_port_number);

    let tx5 = t.connp.tx(4).unwrap();

    assert!(tx5.is_complete());
    assert!(!tx5.flags.is_set(HtpFlags::HOST_AMBIGUOUS));
    assert!(tx5.request_hostname.as_ref().unwrap().eq("www.example.com"));
    assert_eq!(Some(80), tx5.request_port_number);
}

#[test]
fn Http_0_9() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("21-http09.t").is_ok());

    assert_eq!(1, t.connp.tx_size());
    assert!(!t.connp.conn.flags.is_set(ConnectionFlags::HTTP_0_9_EXTRA));

    let _tx = t.connp.tx(0).unwrap();
}

#[test]
fn Http11HostMissing() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("22-http_1_1-host_missing").is_ok());
    assert_eq!(1, t.connp.tx_size());
    let tx = t.connp.tx(0).unwrap();
    assert!(tx.flags.is_set(HtpFlags::HOST_MISSING));
}

#[test]
fn Http_0_9_Multiple() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("23-http09-multiple.t").is_ok());

    assert_eq!(1, t.connp.tx_size());
    assert!(t.connp.conn.flags.is_set(ConnectionFlags::HTTP_0_9_EXTRA));

    let _tx = t.connp.tx(0).unwrap();
}

#[test]
fn Http_0_9_Explicit() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("24-http09-explicit.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(!tx.is_protocol_0_9);
}

#[test]
fn SmallChunks() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("25-small-chunks.t").is_ok());
}

fn ConnectionParsing_RequestHeaderData_REQUEST_HEADER_DATA(d: &mut Data) -> Result<()> {
    unsafe {
        static mut COUNTER: i32 = 0;
        let len = d.len();
        let data: &[u8] = slice::from_raw_parts(d.data(), len);
        match COUNTER {
            0 => {
                if !((len == 11) && data == b"User-Agent:") {
                    eprintln!("Mismatch in chunk 0");
                    COUNTER = -1;
                }
            }
            1 => {
                if !((len == 5) && data == b" Test") {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -1;
                }
            }
            2 => {
                if !((len == 5) && data == b" User") {
                    eprintln!("Mismatch in chunk 2");
                    COUNTER = -1;
                }
            }
            3 => {
                if !((len == 30) && data == b" Agent\nHost: www.example.com\n\n") {
                    eprintln!("Mismatch in chunk 3");
                    COUNTER = -1;
                }
            }
            _ => {
                if COUNTER >= 0 {
                    eprintln!("Seen more than 4 chunks");
                    COUNTER = -1;
                }
            }
        }

        if COUNTER >= 0 {
            COUNTER += 1;
        }

        (*d.tx()).set_user_data(Box::new(COUNTER));
        Ok(())
    }
}

#[test]
fn RequestHeaderData() {
    let mut cfg = TestConfig();
    cfg.register_request_header_data(ConnectionParsing_RequestHeaderData_REQUEST_HEADER_DATA);
    let mut t = Test::new(cfg);
    assert!(t.run("26-request-headers-raw.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_eq!(4, *tx.user_data::<i32>().unwrap());
}

fn ConnectionParsing_RequestTrailerData_REQUEST_TRAILER_DATA(d: &mut Data) -> Result<()> {
    unsafe {
        static mut COUNTER: i32 = 0;
        let len = d.len();
        let data: &[u8] = slice::from_raw_parts(d.data(), len);
        match COUNTER {
            0 => {
                if !((len == 7) && (data == b"Cookie:")) {
                    eprintln!("Mismatch in chunk 0");
                    COUNTER = -1;
                }
            }
            1 => {
                if !((len == 6) && (data == b" 2\r\n\r\n")) {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -2;
                }
            }
            _ => {
                if COUNTER >= 0 {
                    eprintln!("Seen more than 4 chunks");
                    COUNTER = -3;
                }
            }
        }

        if COUNTER >= 0 {
            COUNTER += 1;
        }

        (*d.tx()).set_user_data(Box::new(COUNTER));
        Ok(())
    }
}

#[test]
fn RequestTrailerData() {
    let mut cfg = TestConfig();
    cfg.register_request_trailer_data(ConnectionParsing_RequestTrailerData_REQUEST_TRAILER_DATA);
    let mut t = Test::new(cfg);
    assert!(t.run("27-request-trailer-raw.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_eq!(2, *tx.user_data::<i32>().unwrap());
}

fn ConnectionParsing_ResponseHeaderData_RESPONSE_HEADER_DATA(d: &mut Data) -> Result<()> {
    unsafe {
        static mut COUNTER: i32 = 0;
        let len = d.len();
        let data: &[u8] = slice::from_raw_parts(d.data(), len);
        match COUNTER {
            0 => {
                if !((len == 5) && (data == b"Date:")) {
                    eprintln!("Mismatch in chunk 0");
                    COUNTER = -1;
                }
            }
            1 => {
                if !((len == 5) && (data == b" Mon,")) {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -2;
                }
            }
            2 => {
                if !((len == 34) && (data == " 31 Aug 2009 20:25:50 GMT\r\nServer:".as_bytes())) {
                    eprintln!("Mismatch in chunk 2");
                    COUNTER = -3;
                }
            }
            3 => {
                if !((len == 83) && (data == " Apache\r\nConnection: close\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\n\r\n".as_bytes())) {
                    eprintln!("Mismatch in chunk 3");
                    COUNTER = -4;
                }
            }
            _ => {
                if COUNTER >= 0 {
                    eprintln!("Seen more than 4 chunks");
                    COUNTER = -5;
                }
            }
        }

        if COUNTER >= 0 {
            COUNTER += 1;
        }

        (*d.tx()).set_user_data(Box::new(COUNTER));

        Ok(())
    }
}

#[test]
fn ResponseHeaderData() {
    let mut cfg = TestConfig();
    cfg.register_response_header_data(ConnectionParsing_ResponseHeaderData_RESPONSE_HEADER_DATA);
    let mut t = Test::new(cfg);
    assert!(t.run("28-response-headers-raw.t").is_ok());

    let tx = t.connp.tx(0).unwrap();
    assert_eq!(4, *tx.user_data::<i32>().unwrap());
}

fn ConnectionParsing_ResponseTrailerData_RESPONSE_TRAILER_DATA(d: &mut Data) -> Result<()> {
    unsafe {
        static mut COUNTER: i32 = 0;
        let len = d.len();
        let data: &[u8] = slice::from_raw_parts(d.data(), len);
        match COUNTER {
            0 => {
                if !((len == 11) && (data == b"Set-Cookie:")) {
                    eprintln!("Mismatch in chunk 0");
                    COUNTER = -1;
                }
            }

            1 => {
                if !((len == 6) && (data == b" name=")) {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -2;
                }
            }

            2 => {
                if !((len == 22) && (data == b"value\r\nAnother-Header:")) {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -3;
                }
            }

            3 => {
                if !((len == 17) && (data == b" Header-Value\r\n\r\n")) {
                    eprintln!("Mismatch in chunk 1");
                    COUNTER = -4;
                }
            }

            _ => {
                if COUNTER >= 0 {
                    eprintln!("Seen more than 4 chunks");
                    COUNTER = -5;
                }
            }
        }

        if COUNTER >= 0 {
            COUNTER += 1;
        }

        (*d.tx()).set_user_data(Box::new(COUNTER));
        Ok(())
    }
}

#[test]
fn ResponseTrailerData() {
    let mut cfg = TestConfig();
    cfg.register_response_trailer_data(ConnectionParsing_ResponseTrailerData_RESPONSE_TRAILER_DATA);
    let mut t = Test::new(cfg);
    assert!(t.run("29-response-trailer-raw.t").is_ok());

    let tx = t.connp.tx(0).unwrap();
    assert_eq!(4, *tx.user_data::<i32>().unwrap());
}

#[test]
fn GetIPv6() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("30-get-ipv6.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));

    assert!(tx
        .request_uri
        .as_ref()
        .unwrap()
        .eq("http://[::1]:8080/?p=%20"));

    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .hostname
        .as_ref()
        .unwrap()
        .eq("[::1]"));
    assert_eq!(8080, tx.parsed_uri.as_ref().unwrap().port_number.unwrap());
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .eq("p=%20"));

    assert_contains_param!(&tx.request_params, "p", " ");
}

#[test]
fn GetRequestLineNul() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("31-get-request-line-nul.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_uri.as_ref().unwrap().eq("/?p=%20"));
}

#[test]
fn InvalidHostname1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("32-invalid-hostname.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(tx.flags.is_set(HtpFlags::HOSTH_INVALID));
    assert!(tx.flags.is_set(HtpFlags::HOSTU_INVALID));
    assert!(tx.flags.is_set(HtpFlags::HOST_INVALID));
}

#[test]
fn InvalidHostname2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("33-invalid-hostname.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(!tx.flags.is_set(HtpFlags::HOSTH_INVALID));
    assert!(tx.flags.is_set(HtpFlags::HOSTU_INVALID));
    assert!(tx.flags.is_set(HtpFlags::HOST_INVALID));
}

#[test]
fn InvalidHostname3() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("34-invalid-hostname.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::HOSTH_INVALID));
    assert!(!tx.flags.is_set(HtpFlags::HOSTU_INVALID));
    assert!(tx.flags.is_set(HtpFlags::HOST_INVALID));
}

#[test]
fn EarlyResponse() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("35-early-response.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert!(tx.is_complete());
}

#[test]
fn InvalidRequest1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("36-invalid-request-1-invalid-c-l.t").is_err());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::HEADERS, tx.request_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_INVALID));
    assert!(tx.flags.is_set(HtpFlags::REQUEST_INVALID_C_L));

    assert!(tx.request_hostname.is_some());
}

#[test]
fn InvalidRequest2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("37-invalid-request-2-t-e-and-c-l.t").is_ok());
    // No error, flags only.

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_SMUGGLING));

    assert!(tx.request_hostname.is_some());
}

#[test]
fn InvalidRequest3() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("38-invalid-request-3-invalid-t-e.t").is_err());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::HEADERS, tx.request_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_INVALID));
    assert!(tx.flags.is_set(HtpFlags::REQUEST_INVALID_T_E));

    assert!(tx.request_hostname.is_some());
}

#[test]
fn AutoDestroyCrash() {
    let mut cfg = TestConfig();
    cfg.set_tx_auto_destroy(true);
    let mut t = Test::new(cfg);
    assert!(t.run("39-auto-destroy-crash.t").is_ok());

    assert_eq!(4, t.connp.tx_size());
}

#[test]
fn AuthBasic() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("40-auth-basic.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpAuthType::BASIC, tx.request_auth_type);

    assert!(tx.request_auth_username.as_ref().unwrap().eq("ivanr"));
    assert!(tx.request_auth_password.as_ref().unwrap().eq("secret"));
}

#[test]
fn AuthDigest() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("41-auth-digest.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::DIGEST, tx.request_auth_type);

    assert!(tx.request_auth_username.as_ref().unwrap().eq("ivanr"));

    assert!(tx.request_auth_password.is_none());
}

#[test]
fn Unknown_MethodOnly() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("42-unknown-method_only.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert!(tx.request_method.as_ref().unwrap().eq("HELLO"));

    assert!(tx.request_uri.is_none());

    assert!(tx.is_protocol_0_9);
}

#[test]
fn InvalidHtpProtocol() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("43-invalid-protocol.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpProtocol::INVALID, tx.request_protocol_number);
}

#[test]
fn AuthBasicInvalid() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("44-auth-basic-invalid.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::BASIC, tx.request_auth_type);

    assert!(tx.request_auth_username.is_none());

    assert!(tx.request_auth_password.is_none());

    assert!(tx.flags.is_set(HtpFlags::AUTH_INVALID));
}

#[test]
fn AuthDigestUnquotedUsername() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("45-auth-digest-unquoted-username.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::DIGEST, tx.request_auth_type);

    assert!(tx.request_auth_username.is_none());

    assert!(tx.request_auth_password.is_none());

    assert!(tx.flags.is_set(HtpFlags::AUTH_INVALID));
}

#[test]
fn AuthDigestInvalidUsername1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("46-auth-digest-invalid-username.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::DIGEST, tx.request_auth_type);

    assert!(tx.request_auth_username.is_none());

    assert!(tx.request_auth_password.is_none());

    assert!(tx.flags.is_set(HtpFlags::AUTH_INVALID));
}

#[test]
fn AuthUnrecognized() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("47-auth-unrecognized.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::UNRECOGNIZED, tx.request_auth_type);

    assert!(tx.request_auth_username.is_none());

    assert!(tx.request_auth_password.is_none());
}

#[test]
fn InvalidResponseHeaders1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("48-invalid-response-headers-1.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert_eq!(8, tx.response_headers.size());

    assert_response_header_eq!(tx, "", "No Colon");
    assert_response_header_flag_contains!(tx, "", HtpFlags::FIELD_INVALID);
    assert_response_header_flag_contains!(tx, "", HtpFlags::FIELD_UNPARSEABLE);

    assert_response_header_eq!(tx, "Lws", "After Header Name");
    assert_response_header_flag_contains!(tx, "Lws", HtpFlags::FIELD_INVALID);

    assert_response_header_eq!(tx, "Header@Name", "Not Token");
    assert_response_header_flag_contains!(tx, "Header@Name", HtpFlags::FIELD_INVALID);
}

#[test]
fn InvalidResponseHeaders2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("49-invalid-response-headers-2.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert_eq!(6, tx.response_headers.size());

    assert_response_header_eq!(tx, "", "Empty Name");
    assert_response_header_flag_contains!(tx, "", HtpFlags::FIELD_INVALID);
}

#[test]
fn Util() {
    use htp::{htp_error, htp_log};
    let mut cfg = TestConfig();
    cfg.log_level = HtpLogLevel::NONE;
    let mut t = Test::new(cfg);
    assert!(t.run("50-util.t").is_ok());
    // Explicitly add a log message to verify it is not logged
    htp_error!(&mut t.connp.logger, HtpLogCode::UNKNOWN, "Log message");
    assert_eq!(0, t.connp.conn.get_logs().len());
}

#[test]
fn GetIPv6Invalid() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("51-get-ipv6-invalid.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));

    assert!(tx
        .request_uri
        .as_ref()
        .unwrap()
        .eq("http://[::1:8080/?p=%20"));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .hostname
        .as_ref()
        .unwrap()
        .eq("[::1:8080"));
}

#[test]
fn InvalidPath() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("52-invalid-path.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));

    assert!(tx.request_uri.as_ref().unwrap().eq("invalid/path?p=%20"));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("invalid/path"));
}

#[test]
fn PathUtf8_None() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("53-path-utf8-none.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(!tx.flags.is_set(HtpFlags::PATH_UTF8_VALID));
    assert!(!tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));
    assert!(!tx.flags.is_set(HtpFlags::PATH_HALF_FULL_RANGE));
}

#[test]
fn PathUtf8_Valid() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("54-path-utf8-valid.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_VALID));
}

#[test]
fn PathUtf8_Overlong2() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("55-path-utf8-overlong-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));
}

#[test]
fn PathUtf8_Overlong3() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("56-path-utf8-overlong-3.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));
}

#[test]
fn PathUtf8_Overlong4() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("57-path-utf8-overlong-4.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));
}

#[test]
fn PathUtf8_Invalid() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("58-path-utf8-invalid.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_INVALID));
    assert!(!tx.flags.is_set(HtpFlags::PATH_UTF8_VALID));
}

#[test]
fn PathUtf8_FullWidth() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("59-path-utf8-fullwidth.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_HALF_FULL_RANGE));
}

#[test]
fn PathUtf8_Decode_Valid() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);

    assert!(t.run("54-path-utf8-valid.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/Ristic.txt"));
}

#[test]
fn PathUtf8_Decode_Overlong2() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);
    assert!(t.run("55-path-utf8-overlong-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));

    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/&.txt"));
}

#[test]
fn PathUtf8_Decode_Overlong3() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);

    assert!(t.run("56-path-utf8-overlong-3.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));

    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/&.txt"));
}

#[test]
fn PathUtf8_Decode_Overlong4() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);

    assert!(t.run("57-path-utf8-overlong-4.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_OVERLONG));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/&.txt"));
}

#[test]
fn PathUtf8_Decode_Invalid() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);
    assert!(t.run("58-path-utf8-invalid.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_UTF8_INVALID));
    assert!(!tx.flags.is_set(HtpFlags::PATH_UTF8_VALID));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/Ristic?.txt"));
}

#[test]
fn PathUtf8_Decode_FullWidth() {
    let mut cfg = TestConfig();
    cfg.set_utf8_convert_bestfit(true);
    let mut t = Test::new(cfg);

    assert!(t.run("59-path-utf8-fullwidth.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.flags.is_set(HtpFlags::PATH_HALF_FULL_RANGE));

    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .path
        .as_ref()
        .unwrap()
        .eq("/&.txt"));
}

#[test]
fn RequestCookies1() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("60-request-cookies-1.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(3, tx.request_cookies.size());

    let mut res = &tx.request_cookies[0];
    assert!(res.0.eq("p"));
    assert!(res.1.eq("1"));

    res = &tx.request_cookies[1];
    assert!(res.0.eq("q"));
    assert!(res.1.eq("2"));

    res = &tx.request_cookies[2];
    assert!(res.0.eq("z"));
    assert!(res.1.eq(""));
}

#[test]
fn EmptyLineBetweenRequests() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("61-empty-line-between-requests.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let _tx = t.connp.tx(1).unwrap();

    /*part of previous request body assert_eq!(1, tx.request_ignored_lines);*/
}

#[test]
fn PostNoBody() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("62-post-no-body.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx1.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx1.response_progress);
    assert!(tx1.response_content_type.as_ref().unwrap().eq("text/html"));

    let tx2 = t.connp.tx(1).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx2.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx2.response_progress);
    assert!(tx2.response_content_type.as_ref().unwrap().eq("text/html"));
}

#[test]
fn PostChunkedInvalid1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("63-post-chunked-invalid-1.t").is_err());
}

#[test]
fn PostChunkedInvalid2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("64-post-chunked-invalid-2.t").is_err());
}

#[test]
fn PostChunkedInvalid3() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("65-post-chunked-invalid-3.t").is_err());
}

#[test]
fn PostChunkedSplitChunk() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("66-post-chunked-split-chunk.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_contains_param!(&tx.request_params, "p", "0123456789");
}

#[test]
fn LongRequestLine1() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("67-long-request-line.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx
        .request_uri
        .as_ref()
        .unwrap()
        .eq("/0123456789/0123456789/"));
}

#[test]
fn LongRequestLine2() {
    let mut cfg = TestConfig();
    cfg.set_field_limit(16);
    let mut t = Test::new(cfg);

    assert!(t.run("67-long-request-line.t").is_err());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::LINE, tx.request_progress);
}

#[test]
fn InvalidRequestHeader() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("68-invalid-request-header.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).expect("expected at least one transaction");

    assert_request_header_eq!(tx, "Header-With-NUL", "BEFORE");
}

#[test]
fn TestGenericPersonality() {
    let mut cfg = TestConfig();
    cfg.set_server_personality(HtpServerPersonality::IDS)
        .unwrap();
    let mut t = Test::new(cfg);

    assert!(t.run("02-header-test-apache2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let _tx = t.connp.tx(0).unwrap();
}

#[test]
fn LongResponseHeader() {
    let mut cfg = TestConfig();
    cfg.set_field_limit(18);
    let mut t = Test::new(cfg);

    assert!(t.run("69-long-response-header.t").is_err());

    let tx = t.connp.tx(0).unwrap();

    //error first assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::HEADERS, tx.response_progress);
}

#[test]
fn ResponseInvalidChunkLength() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("70-response-invalid-chunk-length.t").is_ok());
}

#[test]
fn ResponseSplitChunk() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("71-response-split-chunk.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn ResponseBody() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("72-response-split-body.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn ResponseContainsTeAndCl() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("73-response-te-and-cl.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_SMUGGLING));
}

#[test]
fn ResponseMultipleCl() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("74-response-multiple-cl.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_SMUGGLING));

    assert_response_header_eq!(tx, "Content-Length", "12");
    assert_response_header_flag_contains!(tx, "Content-Length", HtpFlags::FIELD_REPEATED);
}

#[test]
fn ResponseMultipleClMismatch() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("88-response-multiple-cl-mismatch.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert!(tx.flags.is_set(HtpFlags::REQUEST_SMUGGLING));

    assert_response_header_eq!(tx, "Content-Length", "12");
    assert_response_header_flag_contains!(tx, "Content-Length", HtpFlags::FIELD_REPEATED);

    let logs = t.connp.conn.get_logs();
    assert_eq!(2, logs.len());
    assert_eq!(logs.get(0).unwrap().msg.msg, "Ambiguous response C-L value");
    assert_eq!(HtpLogLevel::WARNING, logs.get(0).unwrap().msg.level);
    assert_eq!(logs.get(1).unwrap().msg.msg, "Repetition for header");
    assert_eq!(HtpLogLevel::WARNING, logs.get(1).unwrap().msg.level);
}

#[test]
fn ResponseInvalidCl() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("75-response-invalid-cl.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert!(!tx.flags.is_set(HtpFlags::REQUEST_SMUGGLING));
}

#[test]
fn ResponseNoBody() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("76-response-no-body.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx1.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx1.response_progress);

    assert_response_header_eq!(tx1, "Server", "Apache");

    let tx2 = t.connp.tx(1).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx2.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx2.response_progress);

    assert!(tx1 != tx2);
}

#[test]
fn ResponseFoldedHeaders() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("77-response-folded-headers.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let tx1 = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx1.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx1.response_progress);

    assert_response_header_eq!(tx1, "Server", "Apache Server");

    let tx2 = t.connp.tx(1).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx2.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx2.response_progress);
}

#[test]
fn ResponseNoStatusHeaders() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("78-response-no-status-headers.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn ConnectInvalidHostport() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("79-connect-invalid-hostport.t").is_ok());

    assert_eq!(2, t.connp.tx_size());
}

#[test]
fn HostnameInvalid1() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("80-hostname-invalid-1.t").is_ok());

    assert_eq!(1, t.connp.tx_size());
}

#[test]
fn HostnameInvalid2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("81-hostname-invalid-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());
}

#[test]
fn Put() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("82-put.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    let file = t.connp.request_file.as_ref().unwrap();
    assert_eq!(file.len, 12);
    assert_eq!(file.source as u8, HtpFileSource::REQUEST_BODY as u8);
    assert!(file.filename.is_none());
    assert!(file.tmpfile.is_none());

    assert!(tx.request_hostname.as_ref().unwrap().eq("www.example.com"));
}

#[test]
fn AuthDigestInvalidUsername2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("83-auth-digest-invalid-username-2.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::DIGEST, tx.request_auth_type);

    assert!(tx.request_auth_username.is_none());

    assert!(tx.request_auth_password.is_none());

    assert!(tx.flags.is_set(HtpFlags::AUTH_INVALID));
}

#[test]
fn ResponseNoStatusHeaders2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("84-response-no-status-headers-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

// Test was commented out of libhtp
//#[test]
//fn ZeroByteRequestTimeout() {
//    let mut t = Test::new(TestConfig());
//unsafe {
//    assert!(t.run("85-zero-byte-request-timeout.t").is_ok());
//
//    assert_eq!(1, t.connp.tx_size());
//
//    let tx = t.connp.conn.get_tx(0);
//    assert!(!tx.is_null());
//
//    assert_eq!(HtpRequestProgress::NOT_STARTED, tx.request_progress);
//    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
//}}

#[test]
fn PartialRequestTimeout() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("86-partial-request-timeout.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn IncorrectHostAmbiguousWarning() {
    let mut t = Test::new(TestConfig());
    assert!(t
        .run("87-issue-55-incorrect-host-ambiguous-warning.t")
        .is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx
        .parsed_uri_raw
        .as_ref()
        .unwrap()
        .port
        .as_ref()
        .unwrap()
        .eq("443"));
    assert!(tx
        .parsed_uri_raw
        .as_ref()
        .unwrap()
        .hostname
        .as_ref()
        .unwrap()
        .eq("www.example.com"));
    assert_eq!(
        443,
        tx.parsed_uri_raw.as_ref().unwrap().port_number.unwrap()
    );

    assert!(tx.request_hostname.as_ref().unwrap().eq("www.example.com"));

    assert!(!tx.flags.is_set(HtpFlags::HOST_AMBIGUOUS));
}

#[test]
fn GetWhitespace() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("89-get-whitespace.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq(" GET"));
    assert!(tx.request_uri.as_ref().unwrap().eq("/?p=%20"));
    assert!(tx
        .parsed_uri
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .eq("p=%20"));
    assert_contains_param!(&tx.request_params, "p", " ");
}

#[test]
fn RequestUriTooLarge() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("90-request-uri-too-large.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn RequestInvalid() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("91-request-unexpected-body.t").is_ok());

    assert_eq!(2, t.connp.tx_size());

    let mut tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("POST"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    tx = t.connp.tx(1).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::NOT_STARTED, tx.response_progress);
}

#[test]
fn Http_0_9_MethodOnly() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("92-http_0_9-method_only.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert!(tx.request_uri.as_ref().unwrap().eq("/"));
    assert!(tx.is_protocol_0_9);
}

#[test]
fn CompressedResponseDeflateAsGzip() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("93-compressed-response-deflateasgzip.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(755, tx.response_message_len);
    assert_eq!(1433, tx.response_entity_len);
}

#[test]
fn CompressedResponseZlibAsDeflate() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-118.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(tx.is_complete());

    assert_response_header_eq!(
        tx,
        "content-disposition",
        "attachment; filename=\"eicar.txt\""
    );
    assert_response_header_eq!(tx, "content-encoding", "deflate");
    assert_eq!(68, tx.response_entity_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(1, user_data.response_data.len());
    let chunk = &user_data.response_data[0];
    assert_eq!(
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*".as_ref(),
        chunk.as_slice()
    );
}

#[test]
fn CompressedResponseMultiple() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("94-compressed-response-multiple.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(51, tx.response_message_len);
    assert_eq!(25, tx.response_entity_len);
}

#[test]
fn CompressedResponseBombLimitOkay() {
    let mut cfg = TestConfig();
    cfg.compression_options.set_bomb_limit(0);
    let mut t = Test::new(cfg);

    assert!(t.run("14-compressed-response-gzip-chunked.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(28261, tx.response_message_len);
    assert_eq!(159_590, tx.response_entity_len);
}

#[test]
fn CompressedResponseBombLimitExceeded() {
    let mut cfg = TestConfig();
    cfg.compression_options.set_bomb_limit(0);
    cfg.compression_options.set_bomb_ratio(2);
    let mut t = Test::new(cfg);

    assert!(t.run("14-compressed-response-gzip-chunked.t").is_err());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(!tx.is_complete());

    assert_eq!(1208, tx.response_message_len);
    assert_eq!(2608, tx.response_entity_len);
}

#[test]
fn CompressedResponseTimeLimitExceeded() {
    let mut cfg = TestConfig();
    cfg.compression_options.set_time_limit(0);
    let mut t = Test::new(cfg);

    assert!(t.run("14-compressed-response-gzip-chunked.t").is_err());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(!tx.is_complete());

    assert_eq!(1208, tx.response_message_len);
    assert_eq!(2608, tx.response_entity_len);
}

#[test]
fn CompressedResponseGzipAsDeflate() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("95-compressed-response-gzipasdeflate.t").is_ok());
    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(187, tx.response_message_len);
    assert_eq!(225, tx.response_entity_len);
}

#[test]
fn CompressedResponseLzma() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("96-compressed-response-lzma.t").is_ok());
    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(90, tx.response_message_len);
    assert_eq!(68, tx.response_entity_len);
}

#[test]
fn CompressedResponseLzmaDisabled() {
    let mut cfg = TestConfig();
    cfg.compression_options.set_lzma_memlimit(0);
    let mut t = Test::new(cfg);

    assert!(t.run("96-compressed-response-lzma.t").is_ok());
    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();
    assert!(tx.is_complete());

    assert_eq!(90, tx.response_message_len);
    assert_eq!(90, tx.response_entity_len);
}

#[test]
fn CompressedResponseLzmaMemlimit() {
    let mut cfg = TestConfig();
    cfg.compression_options.set_lzma_memlimit(1);
    let mut t = Test::new(cfg);

    assert!(t.run("96-compressed-response-lzma.t").is_ok());
    assert_eq!(1, t.connp.tx_size());
    let tx = t.connp.tx(0).unwrap();
    assert!(tx.is_complete());
    assert_eq!(90, tx.response_message_len);
    assert_eq!(54, tx.response_entity_len);
    assert!(tx.response_message.as_ref().unwrap().eq("ok"));
}

#[test]
fn RequestsCut() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("97-requests-cut.t").is_ok());

    assert_eq!(2, t.connp.tx_size());
    let mut tx = t.connp.tx(0).unwrap();
    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    tx = t.connp.tx(1).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
}

#[test]
fn ResponsesCut() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("98-responses-cut.t").is_ok());

    assert_eq!(2, t.connp.tx_size());
    let mut tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert!(tx.response_status_number.eq_num(200));
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    tx = t.connp.tx(1).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert!(tx.response_status_number.eq_num(200));
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn AuthDigest_EscapedQuote() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("100-auth-digest-escaped-quote.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);

    assert_eq!(HtpAuthType::DIGEST, tx.request_auth_type);

    assert!(tx.request_auth_username.as_ref().unwrap().eq("ivan\"r\""));

    assert!(tx.request_auth_password.is_none());
}

#[test]
fn RequestCookies2() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("101-request-cookies-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(3, tx.request_cookies.size());

    let mut res = &tx.request_cookies[0];
    assert!(res.0.eq("p"));
    assert!(res.1.eq("1"));

    res = &tx.request_cookies[1];
    assert!(res.0.eq("q"));
    assert!(res.1.eq("2"));

    res = &tx.request_cookies[2];
    assert!(res.0.eq("z"));
    assert!(res.1.eq(""));
}

#[test]
fn RequestCookies3() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("102-request-cookies-3.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(3, tx.request_cookies.size());

    let mut res = &tx.request_cookies[0];
    assert!(res.0.eq("a"));
    assert!(res.1.eq("1"));

    res = &tx.request_cookies[1];
    assert!(res.0.eq("b"));
    assert!(res.1.eq("2  "));

    res = &tx.request_cookies[2];
    assert!(res.0.eq("c"));
    assert!(res.1.eq("double=equal"));
}

#[test]
fn RequestCookies4() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("103-request-cookies-4.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(3, tx.request_cookies.size());

    let mut res = &tx.request_cookies[0];
    assert!(res.0.eq("c"));
    assert!(res.1.eq("1"));

    res = &tx.request_cookies[1];
    assert!(res.0.eq("a"));
    assert!(res.1.eq("1  "));

    res = &tx.request_cookies[2];
    assert!(res.0.eq("b"));
    assert!(res.1.eq("2"));
}

#[test]
fn RequestCookies5() {
    let mut t = Test::new(TestConfig());
    // Empty cookie
    assert!(t.run("104-request-cookies-5.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(0, tx.request_cookies.size());
}

#[test]
fn Tunnelled1() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("106-tunnelled-1.t").is_ok());
    assert_eq!(2, t.connp.tx_size());
    let tx1 = t.connp.tx(0).unwrap();

    assert!(tx1.request_method.as_ref().unwrap().eq("CONNECT"));
    let tx2 = t.connp.tx(1).unwrap();

    assert!(tx2.request_method.as_ref().unwrap().eq("GET"));
}

#[test]
fn Expect100() {
    let mut t = Test::new(TestConfig());

    assert!(t.run("105-expect-100.t").is_ok());
    assert_eq!(2, t.connp.tx_size());
    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("PUT"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert!(tx.response_status_number.eq_num(401));
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    let tx = t.connp.tx(1).unwrap();

    assert!(tx.request_method.as_ref().unwrap().eq("POST"));
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert!(tx.response_status_number.eq_num(200));
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn UnknownStatusNumber() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("107-response_unknown_status.t").is_ok());
    assert_eq!(1, t.connp.tx_size());
    let tx = t.connp.tx(0).unwrap();

    assert_eq!(tx.response_status_number, HtpResponseNumber::UNKNOWN);
}

#[test]
fn ResponseHeaderCrOnly() {
    // Content-Length terminated with \r only.
    let mut t = Test::new(TestConfig());
    assert!(t.run("108-response-headers-cr-only.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_eq!(2, tx.response_headers.size());
    // Check response headers
    assert_response_header_eq!(tx, "content-type", "text/html");
    assert_response_header_eq!(tx, "Content-Length", "7");
}

#[test]
fn ResponseHeaderDeformedEOL() {
    // Content-Length terminated with \n\r\r\n\r\n only.
    let mut t = Test::new_with_callbacks();
    assert!(t.run("109-response-headers-deformed-eol.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_eq!(2, tx.response_headers.size());
    // Check response headers
    assert_response_header_eq!(tx, "content-type", "text/html");
    assert_response_header_eq!(tx, "content-length", "6");
    let logs = t.connp.conn.get_logs();
    let log_message_count = logs.len();
    assert_eq!(log_message_count, 1);
    assert_eq!(logs.get(0).unwrap().msg.code, HtpLogCode::DEFORMED_EOL);

    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(1, user_data.response_data.len());
    assert_eq!(b"abcdef".as_ref(), (&user_data.response_data[0]).as_slice());
}

#[test]
fn ResponseFoldedHeaders2() {
    // Space folding char
    let mut t = Test::new(TestConfig());
    assert!(t.run("110-response-folded-headers-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert_response_header_eq!(tx, "Server", "Apache Server");
    assert_eq!(3, tx.response_headers.size());
}

#[test]
fn ResponseHeadersChunked() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("111-response-headers-chunked.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert_eq!(2, tx.response_headers.size());

    assert_response_header_eq!(tx, "content-type", "text/html");
    assert_response_header_eq!(tx, "content-length", "12");
}

#[test]
fn ResponseHeadersChunked2() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("112-response-headers-chunked-2.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    assert_eq!(2, tx.response_headers.size());

    assert_response_header_eq!(tx, "content-type", "text/html");
    assert_response_header_eq!(tx, "content-length", "12");
}

#[test]
fn ResponseMultipartRanges() {
    // This should be is_ok() once multipart/byteranges is handled in response parsing
    let mut t = Test::new(TestConfig());
    assert!(t.run("113-response-multipart-byte-ranges.t").is_err());
}

#[test]
fn Http2Upgrade() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("114-http-2-upgrade.t").is_ok());

    assert_eq!(2, t.connp.tx_size());
    assert!(!t.connp.tx(0).unwrap().is_http_2_upgrade);
    assert!(t.connp.tx(1).unwrap().is_http_2_upgrade);
}

#[test]
fn AuthBearer() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("115-auth-bearer.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpAuthType::BEARER, tx.request_auth_type);

    assert!(tx
        .request_auth_token
        .as_ref()
        .unwrap()
        .eq("mF_9.B5f-4.1JqM"));
}

#[test]
fn HttpCloseHeaders() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("http-close-headers.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert!(tx.request_method.as_ref().unwrap().eq("GET"));
    assert!(tx.request_uri.as_ref().unwrap().eq("/"));

    assert_eq!(HtpProtocol::V1_1, tx.request_protocol_number);
    assert_eq!(HtpProtocol::V1_0, tx.response_protocol_number);

    assert_request_header_eq!(tx, "Host", "100.64.0.200");
    assert_request_header_eq!(tx, "Connection", "keep-alive");
    assert_request_header_eq!(tx, "Accept-Encoding", "gzip, deflate");
    assert_request_header_eq!(tx, "Accept", "*/*");
    assert_request_header_eq!(tx, "User-Agent", "python-requests/2.21.0");
    assert_response_header_eq!(tx, "Server", "ng1nx");

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpStartFromResponse() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("http-start-from-response.t").is_ok());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.request_method.is_none());
    assert_eq!(
        tx.request_uri,
        Some(Bstr::from("/libhtp::request_uri_not_seen"))
    );
    assert!(tx.response_status_number.eq_num(200));

    assert_eq!(HtpProtocol::UNKNOWN, tx.request_protocol_number);
    assert_eq!(HtpProtocol::V1_1, tx.response_protocol_number);

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    let tx = t.connp.tx(1).unwrap();
    assert_eq!(tx.request_method, Some(Bstr::from("GET")));
    assert_eq!(tx.request_uri, Some(Bstr::from("/favicon.ico")));
    assert!(tx.response_status_number.eq_num(404));

    assert_eq!(HtpProtocol::V1_1, tx.request_protocol_number);
    assert_eq!(HtpProtocol::V1_1, tx.response_protocol_number);

    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);

    let logs = t.connp.conn.get_logs();
    assert_eq!(1, logs.len());
    assert_eq!(
        logs.get(0).unwrap().msg.msg,
        "Unable to match response to request"
    );
    assert_eq!(HtpLogLevel::ERROR, logs.get(0).unwrap().msg.level);
}

#[test]
fn RequestCompression() {
    let mut cfg = TestConfig();
    cfg.set_request_decompression(true);
    let mut t = Test::new(cfg);

    assert!(t.run("116-request-compression.t").is_ok());
    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(1355, tx.request_message_len);
    assert_eq!(2614, tx.request_entity_len);
}

#[test]
fn RequestResponseCompression() {
    let mut cfg = TestConfig();
    cfg.set_request_decompression(true);
    let mut t = Test::new(cfg);

    assert!(t.run("117-request-response-compression.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    assert!(tx.is_complete());

    assert_eq!(1355, tx.request_message_len);
    assert_eq!(2614, tx.request_entity_len);

    assert_eq!(51, tx.response_message_len);
    assert_eq!(25, tx.response_entity_len);
}

#[test]
fn Post() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("118-post.t").is_ok());

    assert_eq!(1, t.connp.tx_size());

    let tx = t.connp.tx(0).unwrap();

    let file = t.connp.request_file.as_ref().unwrap();
    assert_eq!(file.len, 12);
    assert_eq!(file.source as u8, HtpFileSource::REQUEST_BODY as u8);
    assert!(file.filename.is_none());
    assert!(file.tmpfile.is_none());

    assert!(tx.request_hostname.as_ref().unwrap().eq("www.example.com"));
}

// Evader Tests
#[test]
fn HttpEvader017() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-017.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/cr-size");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "transfer-encoding", "chunked");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(101, tx.response_message_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(5, user_data.response_data.len());
    assert_eq!(
        b"X5O!P%@AP[4\\PZX".as_ref(),
        (&user_data.response_data[0]).as_slice()
    );
    assert_eq!(
        b"54(P^)7CC)7}$EI".as_ref(),
        (&user_data.response_data[1]).as_slice()
    );
    assert_eq!(
        b"CAR-STANDARD-AN".as_ref(),
        (&user_data.response_data[2]).as_slice()
    );
    assert_eq!(
        b"TIVIRUS-TEST-FI".as_ref(),
        (&user_data.response_data[3]).as_slice()
    );
    assert_eq!(
        b"LE!$H+H*".as_ref(),
        (&user_data.response_data[4]).as_slice()
    );
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpEvader018() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-018.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/lf-size");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "transfer-encoding", "chunked");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(101, tx.response_message_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(5, user_data.response_data.len());
    assert_eq!(
        b"X5O!P%@AP[4\\PZX".as_ref(),
        (&user_data.response_data[0]).as_slice()
    );
    assert_eq!(
        b"54(P^)7CC)7}$EI".as_ref(),
        (&user_data.response_data[1]).as_slice()
    );
    assert_eq!(
        b"CAR-STANDARD-AN".as_ref(),
        (&user_data.response_data[2]).as_slice()
    );
    assert_eq!(
        b"TIVIRUS-TEST-FI".as_ref(),
        (&user_data.response_data[3]).as_slice()
    );
    assert_eq!(
        b"LE!$H+H*".as_ref(),
        (&user_data.response_data[4]).as_slice()
    );
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpEvader044() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-044.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/chunked,http10,do_clen");
    assert_eq!(HtpProtocol::V1_0, tx.response_protocol_number);
    assert!(tx.response_status_number.eq_num(200));
    assert_response_header_eq!(tx, "content-type", "application/octet-stream");
    assert_response_header_eq!(
        tx,
        "content-disposition",
        "attachment; filename=\"eicar.txt\""
    );
    assert_response_header_eq!(tx, "transfer-encoding", "chunked");
    assert_response_header_eq!(tx, "connection", "close");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(68, tx.response_message_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(1, user_data.response_data.len());
    let chunk = &user_data.response_data[0];
    assert_eq!(
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*".as_ref(),
        chunk.as_slice()
    );
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpEvader059() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-059.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/chunkednl-");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader060() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-060.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/nl-nl-chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader061() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-061.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/nl-nl-chunked-nl-");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}
#[test]
fn HttpEvader078() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-078.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/chunked/eicar.txt/chunkedcr-,do_clen");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "transfer-encoding", "chunked");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(68, tx.response_message_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(1, user_data.response_data.len());
    let chunk = &user_data.response_data[0];
    assert_eq!(
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*".as_ref(),
        chunk.as_slice()
    );
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpEvader130() {
    let mut t = Test::new(TestConfig());
    assert!(!t.run("http-evader-130.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(
        tx,
        "/compressed/eicar.txt/ce%3Adeflate-nl-,-nl-deflate-nl-;deflate;deflate"
    );
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-Encoding", "deflate , deflate");
    assert_response_header_eq!(tx, "Content-Length", "75");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(76, tx.response_message_len);
}

#[test]
fn HttpEvader195() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-195.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(
        tx,
        "/compressed/eicar.txt/ce%3Agzip;gzip;replace%3A3,1%7C02;replace%3A10,0=0000"
    );
    assert_response_header_eq!(tx, "Content-Encoding", "gzip");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(90, tx.response_message_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(1, user_data.response_data.len());
    assert_eq!(
        (&user_data.response_data[0]).as_slice(),
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*".as_ref()
    );
}

#[test]
fn HttpEvader274() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-274.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/somehdr;space;chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader284() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-284.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/cr;chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader286() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-286.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/crcronly;chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader287() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-287.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/cr-cronly;chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader297() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-297.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/te%5C015%5C040%3Achunked;do_chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader300() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-300.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/te%5C015%5C012%5C040%5C015%5C012%5C040%3A%5C015%5C012%5C040chunked;do_chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader303() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-303.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/te%3A%5C000chunked;do_chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "\0chunked");
}

#[test]
fn HttpEvader307() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-307.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/te%3A%5C012%5C000chunked;do_chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "\0chunked");
}

#[test]
fn HttpEvader318() {
    let mut t = Test::new(TestConfig());
    assert!(!t.run("http-evader-318.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/ce%5C015%5C012%5C040%3Agzip;do_gzip");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-Encoding", "gzip");
    assert_eq!(68, tx.response_entity_len);
    assert_eq!(89, tx.response_message_len);
}

#[test]
fn HttpEvader320() {
    let mut t = Test::new(TestConfig());
    assert!(!t.run("http-evader-320.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/ce%5C013%3Agzip;do_gzip");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-Encoding", "gzip");
    assert_response_header_eq!(tx, "Content-Length", "88");
    assert_eq!(88, tx.response_entity_len);
    assert_eq!(99, tx.response_message_len);
}

#[test]
fn HttpEvader321() {
    let mut t = Test::new(TestConfig());
    assert!(!t.run("http-evader-321.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/ce%5C014%3Agzip;do_gzip");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-Encoding", "gzip");
    assert_response_header_eq!(tx, "Content-Length", "88");
    assert_eq!(88, tx.response_entity_len);
    assert_eq!(99, tx.response_message_len);
}

#[test]
fn HttpEvader390() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-390.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(
        tx,
        "/broken/eicar.txt/status%3A%5C000HTTP/1.1%28space%29200%28space%29ok;chunked"
    );
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader402() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-402.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/chunked;cr-no-crlf;end-crlflf");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader405() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-405.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/chunked;lfcr-no-crlf;end-crlfcrlf");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader411() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-411.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/end-lfcrcrlf;chunked");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader416() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-416.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/end-lf%5C040lf");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-length", "68");
    assert_eq!(69, tx.response_message_len);
    assert_eq!(69, tx.response_entity_len);
    let user_data = tx.user_data::<MainUserData>().unwrap();
    assert!(user_data.request_data.is_empty());
    assert_eq!(2, user_data.response_data.len());
    assert_eq!(
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*".as_ref(),
        (&user_data.response_data[0]).as_slice()
    );
    assert_eq!(b"\n".as_ref(), (&user_data.response_data[1]).as_slice());
    assert_eq!(HtpRequestProgress::COMPLETE, tx.request_progress);
    assert_eq!(HtpResponseProgress::COMPLETE, tx.response_progress);
}

#[test]
fn HttpEvader419() {
    let mut t = Test::new_with_callbacks();
    assert!(t.run("http-evader-419.t").is_ok());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/chunked;end-lf%5C040lf");
    assert_evader_response!(tx);
    assert_evader_chunked!(tx, "chunked");
}

#[test]
fn HttpEvader423() {
    let mut t = Test::new(TestConfig());
    assert!(t.run("http-evader-423.t").is_err());
    let tx = t.connp.tx(0).unwrap();
    assert_evader_request!(tx, "/broken/eicar.txt/gzip;end-lf%5C040lflf");
    assert_evader_response!(tx);
    assert_response_header_eq!(tx, "Content-Encoding", "gzip");
    assert_response_header_eq!(tx, "Content-length", "88");
    assert_eq!(89, tx.response_message_len);
    assert_eq!(68, tx.response_entity_len);
}
