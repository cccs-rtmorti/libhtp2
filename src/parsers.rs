use crate::error::Result;
use crate::transaction::HtpProtocol;
use crate::{
    bstr::Bstr,
    connection_parser::ConnectionParser,
    table, transaction, util,
    util::{ascii_digits, convert_port, hex_digits, take_ascii_whitespace, validate_hostname},
    HtpStatus,
};
use nom::{
    branch::alt,
    bytes::complete::{is_not, tag, tag_no_case, take_until, take_while},
    combinator::{map, not, opt, peek},
    multi::many0,
    sequence::tuple,
    IResult,
};

/// Parses the content type header, trimming any leading whitespace.
/// Finds the end of the MIME type, using the same approach PHP 5.4.3 uses.
///
/// Returns a tuple of the remaining unparsed header data and the content type
fn content_type() -> impl Fn(&[u8]) -> IResult<&[u8], &[u8]> {
    move |input| {
        map(
            tuple((take_ascii_whitespace(), is_not(";, "))),
            |(_, content_type)| content_type,
        )(input)
    }
}

/// Parses the content type header from the given header value, lowercases it, and stores it in the provided ct bstr.
/// Finds the end of the MIME type, using the same approach PHP 5.4.3 uses.
///
/// Returns HtpStatus::OK if successful; HtpStatus::ERROR if not
pub fn parse_content_type(header: &[u8]) -> Result<Bstr> {
    if let Ok((_, content_type)) = content_type()(header) {
        let mut ct = Bstr::from(content_type);
        ct.make_ascii_lowercase();
        Ok(ct)
    } else {
        Err(HtpStatus::ERROR)
    }
}

/// Parses Content-Length string (positive decimal number).
/// White space is allowed before and after the number.
///
/// Returns Content-Length as a number or None if parsing failed.
pub fn parse_content_length(input: &[u8], connp: Option<&ConnectionParser>) -> Option<i64> {
    if let Ok((trailing_data, (leading_data, content_length))) = ascii_digits()(input) {
        if let Some(connp) = connp {
            if leading_data.len() > 0 {
                // Contains invalid characters! But still attempt to process
                htp_warn!(
                    connp,
                    HtpLogCode::CONTENT_LENGTH_EXTRA_DATA_START,
                    "C-L value with extra data in the beginning"
                );
            }

            if trailing_data.len() > 0 {
                // Ok to have junk afterwards
                htp_warn!(
                    connp,
                    HtpLogCode::CONTENT_LENGTH_EXTRA_DATA_END,
                    "C-L value with extra data in the end"
                );
            }
        }
        if let Ok(content_length) = std::str::from_utf8(content_length) {
            if let Ok(content_length) = i64::from_str_radix(content_length, 10) {
                return Some(content_length);
            }
        }
    }
    None
}

/// Parses chunk length (positive hexadecimal number). White space is allowed before
/// and after the number.parse_chunked_length
///
/// Returns a chunked_length or None if empty.
pub fn parse_chunked_length<'a>(input: &'a [u8]) -> std::result::Result<Option<i32>, &'static str> {
    if let Ok((trailing_data, chunked_length)) = hex_digits()(input) {
        if trailing_data.len() == 0 && chunked_length.len() == 0 {
            return Ok(None);
        }
        if let Ok(chunked_length) = std::str::from_utf8(chunked_length) {
            if let Ok(chunked_length) = i32::from_str_radix(chunked_length, 16) {
                return Ok(Some(chunked_length));
            }
        }
    }
    Err("Invalid Chunk Length")
}

/// Attempts to extract the scheme from a given input URI.
/// e.g. input: http://user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag
/// e.g. output: (//user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag, http)
///
/// Returns a tuple of the unconsumed data and the matched scheme
pub fn scheme<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| {
        // Scheme test: if it doesn't start with a forward slash character (which it must
        // for the contents to be a path or an authority), then it must be the scheme part
        map(
            tuple((peek(not(tag("/"))), take_until(":"), tag(":"))),
            |(_, scheme, _)| scheme,
        )(input)
    }
}

/// Attempts to extract the credentials from a given input URI, assuming the scheme has already been extracted.
/// e.g. input: //user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag
/// e.g. output: (www.example.com:1234/path1/path2?a=b&c=d#frag, (user, pass))
///
/// Returns a tuple of the remaining unconsumed data and a tuple of the matched username and password
pub fn credentials<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], (&'a [u8], Option<&'a [u8]>)> {
    move |input| {
        // Authority test: two forward slash characters and it's an authority.
        // One, three or more slash characters, and it's a path.
        // Note: we only attempt to parse authority if we've seen a scheme.
        let (input, (_, _, credentials, _)) =
            tuple((tag("//"), peek(not(tag("/"))), take_until("@"), tag("@")))(input)?;
        let (password, username) = opt(tuple((take_until(":"), tag(":"))))(credentials)?;
        if let Some((username, _)) = username {
            Ok((input, (username, Some(password))))
        } else {
            Ok((input, (credentials, None)))
        }
    }
}

/// Attempts to extract an IPv6 hostname from a given input URI,
/// assuming any scheme, credentials, hostname, port, and path have been already parsed out.
/// e.g. input: [:::]/path1?a=b&c=d#frag
/// e.g. output: ([:::], /path?a=b&c=d#frag)
///
/// Returns a tuple of the remaining unconsumed data and the matched ipv6 hostname
pub fn ipv6<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| -> IResult<&'a [u8], &'a [u8]> {
        let (rest, (_, _, _)) = tuple((tag("["), is_not("/?#]"), opt(tag("]"))))(input)?;
        Ok((rest, &input[..input.len() - rest.len()]))
    }
}

/// Attempts to extract the hostname from a given input URI
/// e.g. input: http://user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag
/// e.g. output: (:1234/path1/path2?a=b&c=d#frag, www.example.com)
///
/// Returns a tuple of the remaining unconsumed data and the matched hostname
pub fn hostname<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| {
        let (input, mut hostname) = map(
            tuple((
                opt(tag("//")), //If it starts with "//", skip (might have parsed a scheme and no creds)
                peek(not(tag("/"))), //If it starts with '/', this is a path, not a hostname
                many0(tag(" ")),
                alt((ipv6(), is_not("/?#:"))),
            )),
            |(_, _, _, hostname)| hostname,
        )(input)?;
        //There may be spaces in the middle of a hostname, so much trim only at the end
        while hostname.ends_with(&[' ' as u8]) {
            hostname = &hostname[..hostname.len() - 1];
        }
        Ok((input, hostname))
    }
}

/// Attempts to extract the port from a given input URI,
/// assuming any scheme, credentials, or hostname have been already parsed out.
/// e.g. input: :1234/path1/path2?a=b&c=d#frag
/// e.g. output: (/path1/path2?a=b&c=d#frag, 1234)
///
/// Returns a tuple of the remaining unconsumed data and the matched port
pub fn port<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| {
        // Must start with ":" for there to be a port to parse
        let (input, (_, _, port, _)) =
            tuple((tag(":"), many0(tag(" ")), is_not("/?#"), many0(tag(" "))))(input)?;
        let (_, port) = is_not(" ")(port)?; //we assume there never will be a space in the middle of a port
        Ok((input, port))
    }
}

/// Attempts to extract the path from a given input URI,
/// assuming any scheme, credentials, hostname, and port have been already parsed out.
/// e.g. input: /path1/path2?a=b&c=d#frag
/// e.g. output: (?a=b&c=d#frag, /path1/path2)
///
/// Returns a tuple of the remaining unconsumed data and the matched path
pub fn path<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| is_not("#?")(input)
}

/// Attempts to extract the query from a given input URI,
/// assuming any scheme, credentials, hostname, port, and path have been already parsed out.
/// e.g. input: ?a=b&c=d#frag
/// e.g. output: (#frag, ?a=b&c=d)
///
/// Returns a tuple of the remaining unconsumed data and the matched query
pub fn query<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| {
        // Skip the starting '?'
        map(tuple((tag("?"), is_not("#"))), |(_, query)| query)(input)
    }
}

/// Attempts to extract the fragment from a given input URI,
/// assuming any other components have been parsed out
/// e.g. input: ?a=b&c=d#frag
/// e.g. output: ("", #frag)
///
/// Returns a tuple of the remaining unconsumed data and the matched fragment
pub fn fragment<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], &'a [u8]> {
    move |input| {
        // Skip the starting '#'
        let (input, _) = tag("#")(input)?;
        Ok((b"", input))
    }
}
/// Parses an authority string, which consists of a hostname with an optional port number
///
/// Returns a remaining unparsed data, parsed hostname, parsed port, converted port number,
/// and a flag indicating whether the parsed data is valid
pub fn parse_hostport(input: &[u8]) -> IResult<&[u8], (&[u8], Option<(&[u8], Option<u16>)>, bool)> {
    let (input, host) = hostname()(input)?;
    let mut valid = validate_hostname(host);
    if let Ok((_, p)) = port()(input) {
        if let Some(port) = convert_port(p) {
            return Ok((input, (host, Some((p, Some(port))), valid)));
        } else {
            return Ok((input, (host, Some((p, None)), false)));
        }
    } else if input.len() > 0 {
        //Trailing data after the hostname that is invalid e.g. [::1]xxxxx
        valid = false;
    }
    Ok((input, (host, None, valid)))
}

/// Extracts the version protocol from the input slice.
///
/// Returns (any unparsed trailing data, (version_number, flag indicating whether input contains trailing and/or leading whitespace and/or leading zeros))
pub fn protocol_version<'a>(input: &'a [u8]) -> IResult<&'a [u8], (&'a [u8], bool)> {
    let (remaining, (_, _, leading, _, trailing, version, _)) = tuple((
        util::take_ascii_whitespace(),
        tag_no_case("HTTP"),
        util::take_ascii_whitespace(),
        tag("/"),
        take_while(|c: u8| c.is_ascii_whitespace() || c == '0' as u8),
        alt((tag(".9"), tag("1.0"), tag("1.1"))),
        util::take_ascii_whitespace(),
    ))(input)?;
    Ok((
        remaining,
        (version, leading.len() > 0 || trailing.len() > 0),
    ))
}

/// Determines protocol number from a textual representation (i.e., "HTTP/1.1"). This
/// function tries to be flexible, allowing whitespace before and after the forward slash,
/// as well as allowing leading zeros in the version number. If such leading/trailing
/// characters are discovered, however, a warning will be logged.
///
/// Returns HtpProtocol version or invalid.
pub fn parse_protocol<'a>(input: &'a [u8], connp: &ConnectionParser) -> HtpProtocol {
    if let Ok((remaining, (version, contains_trailing))) = protocol_version(input) {
        if remaining.len() > 0 {
            return HtpProtocol::INVALID;
        }
        if contains_trailing {
            htp_warn!(
                    connp,
                    HtpLogCode::PROTOCOL_CONTAINS_EXTRA_DATA,
                    "HtpProtocol version contains leading and/or trailing whitespace and/or leading zeros"
                )
        }
        match version {
            b".9" => HtpProtocol::V0_9,
            b"1.0" => HtpProtocol::V1_0,
            b"1.1" => HtpProtocol::V1_1,
            _ => HtpProtocol::INVALID,
        }
    } else {
        HtpProtocol::INVALID
    }
}

/// Determines the numerical value of a response status given as a string.
///
/// Returns HtpStatus code as a u16 on success or None on failure
pub fn parse_status(status: &[u8]) -> Option<u16> {
    if let Ok((trailing_data, (leading_data, status_code))) = util::ascii_digits()(status) {
        if trailing_data.len() > 0 || leading_data.len() > 0 {
            //There are invalid characters in the status code
            return None;
        }
        if let Ok(status_code) = std::str::from_utf8(status_code) {
            if let Ok(status_code) = u16::from_str_radix(status_code, 10) {
                if status_code >= 100 && status_code <= 999 {
                    return Some(status_code);
                }
            }
        }
    }
    None
}

/// Parses Digest Authorization request header.
fn parse_authorization_digest<'a>(auth_header_value: &'a [u8]) -> IResult<&'a [u8], Vec<u8>> {
    // Extract the username
    let (mut remaining_input, _) = tuple((
        take_until("username="),
        tag("username="),
        take_while(|c: u8| c.is_ascii_whitespace()), // allow leading whitespace
        tag("\""), // First character after LWS must be a double quote
    ))(auth_header_value)?;
    let mut result = Vec::new();
    // Unescape any escaped double quotes and find the closing quote
    loop {
        let (remaining, (auth_header, _)) = tuple((take_until("\""), tag("\"")))(remaining_input)?;
        remaining_input = remaining;
        result.extend_from_slice(auth_header);
        if result.last() == Some(&('\\' as u8)) {
            // Remove the escape and push back the double quote
            result.pop();
            result.push('\"' as u8);
        } else {
            // We found the closing double quote!
            break;
        }
    }
    Ok((remaining_input, result))
}

/// Parses Basic Authorization request header.
pub fn parse_authorization_basic(
    in_tx: &mut transaction::Transaction,
    auth_header: &transaction::Header,
) -> Result<()> {
    let data = &auth_header.value;

    if data.len() <= 5 {
        return Err(HtpStatus::DECLINED);
    };

    // Skip 'Basic<lws>'
    let value_start = if let Some(pos) = data[5..].iter().position(|&c| !c.is_ascii_whitespace()) {
        pos + 5
    } else {
        return Err(HtpStatus::DECLINED);
    };

    // Decode base64-encoded data
    let decoded = if let Ok(decoded) = base64::decode(&data[value_start..]) {
        decoded
    } else {
        return Err(HtpStatus::DECLINED);
    };

    // Extract username and password
    let i = if let Some(i) = decoded.iter().position(|&c| c == ':' as u8) {
        i
    } else {
        return Err(HtpStatus::DECLINED);
    };

    let (username, password) = decoded.split_at(i);
    in_tx.request_auth_username = Some(Bstr::from(username));
    in_tx.request_auth_password = Some(Bstr::from(&password[1..]));

    Ok(())
}

/// Parses Authorization request header.
pub fn parse_authorization(in_tx: &mut transaction::Transaction) -> Result<()> {
    let auth_header =
        if let Some((_, auth_header)) = in_tx.request_headers.get_nocase_nozero("authorization") {
            auth_header.clone()
        } else {
            in_tx.request_auth_type = transaction::HtpAuthType::NONE;
            return Ok(());
        };
    // TODO Need a flag to raise when failing to parse authentication headers.
    if auth_header.value.starts_with_nocase("basic") {
        // Basic authentication
        in_tx.request_auth_type = transaction::HtpAuthType::BASIC;
        return parse_authorization_basic(in_tx, &auth_header);
    } else if auth_header.value.starts_with_nocase("digest") {
        // Digest authentication
        in_tx.request_auth_type = transaction::HtpAuthType::DIGEST;
        if let Ok((_, auth_username)) = parse_authorization_digest(auth_header.value.as_slice()) {
            if let Some(username) = &mut in_tx.request_auth_username {
                username.clear();
                username.add(auth_username);
                return Ok(());
            } else {
                in_tx.request_auth_username = Some(Bstr::from(auth_username));
            }
        }
        return Err(HtpStatus::DECLINED);
    } else {
        // Unrecognized authentication method
        in_tx.request_auth_type = transaction::HtpAuthType::UNRECOGNIZED
    }
    Ok(())
}

/// Parses a single v0 request cookie.
///
/// Returns the (name, value).
pub fn single_cookie_v0(data: &[u8]) -> (&[u8], &[u8]) {
    let parts: Vec<&[u8]> = data.splitn(2, |&x| x == '=' as u8).collect();
    match parts.len() {
        1 => (data, b""),
        2 => (parts[0], parts[1]),
        _ => (b"", b""),
    }
}

/// Parses the Cookie request header in v0 format and places the results into tx->request_cookies.
///
/// Returns OK on success, ERROR on error
pub fn parse_cookies_v0(in_tx: &mut transaction::Transaction) -> Result<()> {
    if let Some((_, cookie_header)) = in_tx.request_headers.get_nocase_nozero_mut("cookie") {
        let data: &[u8] = cookie_header.value.as_ref();
        // Create a new table to store cookies.
        in_tx.request_cookies = table::Table::with_capacity(4);
        for cookie in data.split(|b| *b == ';' as u8) {
            if let Ok((cookie, _)) = util::take_ascii_whitespace()(cookie) {
                if cookie.is_empty() {
                    continue;
                }
                let (name, value) = single_cookie_v0(cookie);
                if !name.is_empty() {
                    in_tx
                        .request_cookies
                        .add(Bstr::from(name), Bstr::from(value));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn ParseSingleCookieV0() {
    assert_eq!(
        (b"yummy_cookie".as_ref(), b"choco".as_ref()),
        single_cookie_v0(b"yummy_cookie=choco")
    );
    assert_eq!(
        (b"".as_ref(), b"choco".as_ref()),
        single_cookie_v0(b"=choco")
    );
    assert_eq!(
        (b"yummy_cookie".as_ref(), b"".as_ref()),
        single_cookie_v0(b"yummy_cookie=")
    );
    assert_eq!((b"".as_ref(), b"".as_ref()), single_cookie_v0(b"="));
    assert_eq!((b"".as_ref(), b"".as_ref()), single_cookie_v0(b""));
}

#[test]
fn AuthDigest() {
    assert_eq!(
        b"ivan\"r\"".to_vec(),
        parse_authorization_digest(b"   username=   \"ivan\\\"r\\\"\"")
            .unwrap()
            .1
    );
    assert_eq!(
        b"ivan\"r\"".to_vec(),
        parse_authorization_digest(b"username=\"ivan\\\"r\\\"\"")
            .unwrap()
            .1
    );
    assert_eq!(
        b"ivan\"r\"".to_vec(),
        parse_authorization_digest(b"username=\"ivan\\\"r\\\"\"   ")
            .unwrap()
            .1
    );
    assert_eq!(
        b"ivanr".to_vec(),
        parse_authorization_digest(b"username=\"ivanr\"   ")
            .unwrap()
            .1
    );
    assert_eq!(
        b"ivanr".to_vec(),
        parse_authorization_digest(b"username=   \"ivanr\"   ")
            .unwrap()
            .1
    );
    assert!(parse_authorization_digest(b"username=ivanr\"   ").is_err()); //Missing opening quote
    assert!(parse_authorization_digest(b"username=\"ivanr   ").is_err()); //Missing closing quote
}

#[test]
fn ParseStatus() {
    assert_eq!(Some(200u16), parse_status(&Bstr::from("   200    ")));
    assert_eq!(Some(404u16), parse_status(&Bstr::from("  \t 404    ")));
    assert_eq!(Some(123u16), parse_status(&Bstr::from("123")));
    assert!(parse_status(&Bstr::from("99")).is_none());
    assert!(parse_status(&Bstr::from("1000")).is_none());
    assert!(parse_status(&Bstr::from("200 OK")).is_none());
    assert!(parse_status(&Bstr::from("NOT 200")).is_none());
}

#[test]
fn ParseScheme_1() {
    let i: &[u8] = b"http://user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"//user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag";
    let e: &[u8] = b"http";
    let (left, scheme) = scheme()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(scheme, e);
}

#[test]
fn ParseInvalidScheme() {
    let i: &[u8] = b"/http://user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag";
    assert!(!scheme()(i).is_ok());
}

#[test]
fn ParseCredentials_1() {
    let i: &[u8] = b"//user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"www.example.com:1234/path1/path2?a=b&c=d#frag";
    let u: &[u8] = b"user";
    let p: &[u8] = b"pass";
    let (left, (user, pass)) = credentials()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(user, u);
    assert_eq!(pass.unwrap(), p);
}

#[test]
fn ParseCredentials_2() {
    let i: &[u8] = b"//user@www.example.com:1234/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"www.example.com:1234/path1/path2?a=b&c=d#frag";
    let u: &[u8] = b"user";
    let (left, (user, pass)) = credentials()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(user, u);
    assert!(pass.is_none());
}

#[test]
fn ParseInvalidCredentials() {
    //Must have already parsed the scheme!
    let i: &[u8] = b"http://user:pass@www.example.com:1234/path1/path2?a=b&c=d#frag";
    assert!(!credentials()(i).is_ok());
}

#[test]
fn ParseHostname_1() {
    let i: &[u8] = b"www.example.com:1234/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b":1234/path1/path2?a=b&c=d#frag";
    let e: &[u8] = b"www.example.com";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_2() {
    let i: &[u8] = b"www.example.com/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"/path1/path2?a=b&c=d#frag";
    let e: &[u8] = b"www.example.com";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_3() {
    let i: &[u8] = b"www.example.com?a=b&c=d#frag";
    let o: &[u8] = b"?a=b&c=d#frag";
    let e: &[u8] = b"www.example.com";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_4() {
    let i: &[u8] = b"www.example.com#frag";
    let o: &[u8] = b"#frag";
    let e: &[u8] = b"www.example.com";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_5() {
    let i: &[u8] = b"[::1]:8080";
    let o: &[u8] = b":8080";
    let e: &[u8] = b"[::1]";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_6() {
    let i: &[u8] = b"[::1";
    let o: &[u8] = b"";
    let e: &[u8] = b"[::1";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_7() {
    let i: &[u8] = b"[::1/path1[0]";
    let o: &[u8] = b"/path1[0]";
    let e: &[u8] = b"[::1";
    let (left, hostname) = hostname()(i).unwrap();

    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseHostname_8() {
    let i: &[u8] = b"[::1]xxxx";
    let o: &[u8] = b"xxxx";
    let e: &[u8] = b"[::1]";
    let (left, hostname) = hostname()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(hostname, e);
}

#[test]
fn ParseInvalidHostname() {
    //If it starts with '/' we treat it as a path
    let i: &[u8] = b"/www.example.com/path1/path2?a=b&c=d#frag";
    assert!(!hostname()(i).is_ok());
}

#[test]
fn ParsePort_1() {
    let i: &[u8] = b":1234/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"/path1/path2?a=b&c=d#frag";
    let e: &[u8] = b"1234";
    let (left, path) = port()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePort_2() {
    let i: &[u8] = b":1234?a=b&c=d#frag";
    let o: &[u8] = b"?a=b&c=d#frag";
    let e: &[u8] = b"1234";
    let (left, path) = port()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePort_3() {
    let i: &[u8] = b":1234#frag";
    let o: &[u8] = b"#frag";
    let e: &[u8] = b"1234";
    let (left, path) = port()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePath_1() {
    let i: &[u8] = b"/path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"?a=b&c=d#frag";
    let e: &[u8] = b"/path1/path2";
    let (left, path) = path()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePath_2() {
    let i: &[u8] = b"/path1/path2#frag";
    let o: &[u8] = b"#frag";
    let e: &[u8] = b"/path1/path2";
    let (left, path) = path()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePath_3() {
    let i: &[u8] = b"path1/path2?a=b&c=d#frag";
    let o: &[u8] = b"?a=b&c=d#frag";
    let e: &[u8] = b"path1/path2";
    let (left, path) = path()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParsePath_4() {
    let i: &[u8] = b"//";
    let o: &[u8] = b"";
    let e: &[u8] = b"//";
    let (left, path) = path()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(path, e);
}

#[test]
fn ParseQuery_1() {
    let i: &[u8] = b"?a=b&c=d#frag";
    let o: &[u8] = b"#frag";
    let e: &[u8] = b"a=b&c=d";
    let (left, query) = query()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(query, e);
}

#[test]
fn ParseQuery_2() {
    let i: &[u8] = b"?a=b&c=d";
    let o: &[u8] = b"";
    let e: &[u8] = b"a=b&c=d";
    let (left, query) = query()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(query, e);
}

#[test]
fn ParseFragment() {
    let i: &[u8] = b"#frag";
    let o: &[u8] = b"";
    let e: &[u8] = b"frag";
    let (left, fragment) = fragment()(i).unwrap();
    assert_eq!(left, o);
    assert_eq!(fragment, e);
}

#[test]
fn ParseHostPort_1() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(valid);
}

#[test]
fn ParseHostPort_2() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b" www.example.com ").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(valid);
}

#[test]
fn ParseHostPort_3() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b" www.example.com:8001 ").unwrap();

    assert!(e.eq_nocase(host));
    assert_eq!(8001, port.unwrap().1.unwrap());
    assert!(valid);
}

#[test]
fn ParseHostPort_4() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b" www.example.com :  8001 ").unwrap();

    assert!(e.eq_nocase(host));
    assert_eq!(8001, port.unwrap().1.unwrap());
    assert!(valid);
}

#[test]
fn ParseHostPort_5() {
    let e = Bstr::from("www.example.com.");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com.").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(valid);
}

#[test]
fn ParseHostPort_6() {
    let e = Bstr::from("www.example.com.");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com.:8001").unwrap();

    assert!(e.eq_nocase(host));
    assert_eq!(8001, port.unwrap().1.unwrap());
    assert!(valid);
}

#[test]
fn ParseHostPort_7() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com:").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_8() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com:ff").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.unwrap().1.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_9() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com:0").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.unwrap().1.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_10() {
    let e = Bstr::from("www.example.com");
    let (_, (host, port, valid)) = parse_hostport(b"www.example.com:65536").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.unwrap().1.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_11() {
    let e = Bstr::from("[::1]");
    let (_, (host, port, valid)) = parse_hostport(b"[::1]:8080").unwrap();

    assert!(e.eq_nocase(host));
    assert_eq!(8080, port.unwrap().1.unwrap());
    assert!(valid);
}

#[test]
fn ParseHostPort_12() {
    let e = Bstr::from("[::1]");
    let (_, (host, port, valid)) = parse_hostport(b"[::1]:").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_13() {
    let e = Bstr::from("[::1]");
    let (_, (host, port, valid)) = parse_hostport(b"[::1]x").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(!valid);
}

#[test]
fn ParseHostPort_14() {
    let e = Bstr::from("[::1");
    let (_, (host, port, valid)) = parse_hostport(b"[::1").unwrap();

    assert!(e.eq_nocase(host));
    assert!(port.is_none());
    assert!(!valid);
}

#[test]
fn ParseContentLength() {
    assert_eq!(134, parse_content_length(b"134", None).unwrap());
    assert_eq!(134, parse_content_length(b"    \t134    ", None).unwrap());
    assert_eq!(134, parse_content_length(b"abcd134    ", None).unwrap());
    assert!(parse_content_length(b"abcd    ", None).is_none());
}

#[test]
fn ParseChunkedLength() {
    assert_eq!(Ok(Some(0x12a5)), parse_chunked_length(b"12a5"));
    assert_eq!(Ok(Some(0x12a5)), parse_chunked_length(b"    \t12a5    "));
}

#[test]
fn ParseContentType() {
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"multipart/form-data").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"multipart/form-data;boundary=X").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"multipart/form-data boundary=X").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"multipart/form-data,boundary=X").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"multipart/FoRm-data").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data\t"),
        parse_content_type(b"multipart/form-data\t boundary=X").unwrap()
    );
    assert_eq!(
        Bstr::from("multipart/form-data"),
        parse_content_type(b"   \tmultipart/form-data boundary=X").unwrap()
    );
}