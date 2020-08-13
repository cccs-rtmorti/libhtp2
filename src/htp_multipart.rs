use crate::bstr::{bstr_len, bstr_ptr};
use crate::htp_util::{take_ascii_whitespace, Flags};
use crate::{
    bstr, bstr_builder, htp_config, htp_hooks, htp_table, htp_transaction, htp_util, list, Status,
};
use bitflags;
use std::cmp::Ordering;

use nom::{
    branch::alt,
    bytes::complete::{tag, tag_no_case, take, take_until, take_while, take_while1},
    character::complete::char,
    character::is_space,
    combinator::{map, not, opt, peek},
    multi::fold_many1,
    sequence::tuple,
    IResult,
};

bitflags::bitflags! {
    #[repr(C)]
    pub struct MultipartFlags: u64 {

/// Seen a LF line in the payload. LF lines are not allowed, but
/// some clients do use them and some backends do accept them. Mixing
/// LF and CRLF lines within some payload might be unusual.
        const HTP_MULTIPART_LF_LINE = 0x0001;
/// Seen a CRLF line in the payload. This is normal and expected.
        const HTP_MULTIPART_CRLF_LINE = 0x0002;
/// Seen LWS after a boundary instance in the body. Unusual.
        const HTP_MULTIPART_BBOUNDARY_LWS_AFTER = 0x0004;
/// Seen non-LWS content after a boundary instance in the body. Highly unusual.
        const HTP_MULTIPART_BBOUNDARY_NLWS_AFTER = 0x0008;

/// Payload has a preamble part. Might not be that unusual.
        const HTP_MULTIPART_HAS_PREAMBLE = 0x0010;

/// Payload has an epilogue part. Unusual.
        const HTP_MULTIPART_HAS_EPILOGUE = 0x0020;

/// The last boundary was seen in the payload. Absence of the last boundary
/// may not break parsing with some (most?) backends, but it means that the payload
/// is not well formed. Can occur if the client gives up, or if the connection is
/// interrupted. Incomplete payloads should be blocked whenever possible.
        const HTP_MULTIPART_SEEN_LAST_BOUNDARY = 0x0040;

/// There was a part after the last boundary. This is highly irregular
/// and indicative of evasion.
        const HTP_MULTIPART_PART_AFTER_LAST_BOUNDARY = 0x0080;

/// The payloads ends abruptly, without proper termination. Can occur if the client gives up,
/// or if the connection is interrupted. When this flag is raised, HTP_MULTIPART_PART_INCOMPLETE
/// will also be raised for the part that was only partially processed. (But the opposite may not
/// always be the case -- there are other ways in which a part can be left incomplete.)
        const HTP_MULTIPART_INCOMPLETE = 0x0100;
/// The boundary in the Content-Type header is invalid.
        const HTP_MULTIPART_HBOUNDARY_INVALID = 0x0200;

/// The boundary in the Content-Type header is unusual. This may mean that evasion
/// is attempted, but it could also mean that we have encountered a client that does
/// not do things in the way it should.
        const HTP_MULTIPART_HBOUNDARY_UNUSUAL = 0x0400;

/// The boundary in the Content-Type header is quoted. This is very unusual,
/// and may be indicative of an evasion attempt.
        const HTP_MULTIPART_HBOUNDARY_QUOTED = 0x0800;
/// Header folding was used in part headers. Very unusual.
        const HTP_MULTIPART_PART_HEADER_FOLDING = 0x1000;

/// A part of unknown type was encountered, which probably means that the part is lacking
/// a Content-Disposition header, or that the header is invalid. Highly unusual.
        const HTP_MULTIPART_PART_UNKNOWN = 0x2000;
/// There was a repeated part header, possibly in an attempt to confuse the parser. Very unusual.
        const HTP_MULTIPART_PART_HEADER_REPEATED = 0x4000;
/// Unknown part header encountered.
        const HTP_MULTIPART_PART_HEADER_UNKNOWN = 0x8000;
/// Invalid part header encountered.
        const HTP_MULTIPART_PART_HEADER_INVALID = 0x10000;
/// Part type specified in the C-D header is neither MULTIPART_PART_TEXT nor MULTIPART_PART_FILE.
        const HTP_MULTIPART_CD_TYPE_INVALID = 0x20000;
/// Content-Disposition part header with multiple parameters with the same name.
        const HTP_MULTIPART_CD_PARAM_REPEATED = 0x40000;
/// Unknown Content-Disposition parameter.
        const HTP_MULTIPART_CD_PARAM_UNKNOWN = 0x80000;
/// Invalid Content-Disposition syntax.
        const HTP_MULTIPART_CD_SYNTAX_INVALID = 0x100000;

/// There is an abruptly terminated part. This can happen when the payload itself is abruptly
/// terminated (in which case HTP_MULTIPART_INCOMPLETE) will be raised. However, it can also
/// happen when a boundary is seen before any part data.
        const HTP_MULTIPART_PART_INCOMPLETE = 0x200000;
/// A NUL byte was seen in a part header area.
        const HTP_MULTIPART_NUL_BYTE = 0x400000;
/// A collection of flags that all indicate an invalid C-D header.
        const HTP_MULTIPART_CD_INVALID = ( Self::HTP_MULTIPART_CD_TYPE_INVALID.bits | Self::HTP_MULTIPART_CD_PARAM_REPEATED.bits | Self::HTP_MULTIPART_CD_PARAM_UNKNOWN.bits | Self::HTP_MULTIPART_CD_SYNTAX_INVALID.bits );
/// A collection of flags that all indicate an invalid part.
        const HTP_MULTIPART_PART_INVALID = ( Self::HTP_MULTIPART_CD_INVALID.bits | Self::HTP_MULTIPART_NUL_BYTE.bits | Self::HTP_MULTIPART_PART_UNKNOWN.bits | Self::HTP_MULTIPART_PART_HEADER_REPEATED.bits | Self::HTP_MULTIPART_PART_INCOMPLETE.bits | Self::HTP_MULTIPART_PART_HEADER_UNKNOWN.bits | Self::HTP_MULTIPART_PART_HEADER_INVALID.bits );
/// A collection of flags that all indicate an invalid Multipart payload.
        const HTP_MULTIPART_INVALID = ( Self::HTP_MULTIPART_PART_INVALID.bits | Self::HTP_MULTIPART_PART_AFTER_LAST_BOUNDARY.bits | Self::HTP_MULTIPART_INCOMPLETE.bits | Self::HTP_MULTIPART_HBOUNDARY_INVALID.bits );
/// A collection of flags that all indicate an unusual Multipart payload.
        const HTP_MULTIPART_UNUSUAL = ( Self::HTP_MULTIPART_INVALID.bits | Self::HTP_MULTIPART_PART_HEADER_FOLDING.bits | Self::HTP_MULTIPART_BBOUNDARY_NLWS_AFTER.bits | Self::HTP_MULTIPART_HAS_EPILOGUE.bits | Self::HTP_MULTIPART_HBOUNDARY_UNUSUAL.bits | Self::HTP_MULTIPART_HBOUNDARY_QUOTED.bits );
/// A collection of flags that all indicate an unusual Multipart payload, with a low sensitivity to irregularities.
        const HTP_MULTIPART_UNUSUAL_PARANOID = ( Self::HTP_MULTIPART_UNUSUAL.bits | Self::HTP_MULTIPART_LF_LINE.bits | Self::HTP_MULTIPART_BBOUNDARY_LWS_AFTER.bits | Self::HTP_MULTIPART_HAS_PREAMBLE.bits );
    }
}
extern "C" {
    #[no_mangle]
    fn malloc(_: libc::size_t) -> *mut core::ffi::c_void;
    #[no_mangle]
    fn calloc(_: libc::size_t, _: libc::size_t) -> *mut core::ffi::c_void;
    #[no_mangle]
    fn free(__ptr: *mut core::ffi::c_void);
    #[no_mangle]
    fn mkstemp(__template: *mut libc::c_char) -> libc::c_int;
    #[no_mangle]
    fn close(__fd: libc::c_int) -> libc::c_int;
    #[no_mangle]
    fn write(
        __fd: libc::c_int,
        __buf: *const core::ffi::c_void,
        __n: libc::size_t,
    ) -> libc::ssize_t;
    #[no_mangle]
    fn unlink(__name: *const libc::c_char) -> libc::c_int;
    #[no_mangle]
    fn umask(__mask: libc::c_uint) -> libc::c_uint;
    #[no_mangle]
    fn memcmp(
        _: *const core::ffi::c_void,
        _: *const core::ffi::c_void,
        _: libc::size_t,
    ) -> libc::c_int;
    #[no_mangle]
    fn memchr(
        _: *const core::ffi::c_void,
        _: libc::c_int,
        _: libc::size_t,
    ) -> *mut core::ffi::c_void;
    #[no_mangle]
    fn strncpy(_: *mut libc::c_char, _: *const libc::c_char, _: libc::size_t) -> *mut libc::c_char;
    #[no_mangle]
    fn strncat(_: *mut libc::c_char, _: *const libc::c_char, _: libc::size_t) -> *mut libc::c_char;
    #[no_mangle]
    fn strdup(_: *const libc::c_char) -> *mut libc::c_char;
    #[no_mangle]
    fn strlen(_: *const libc::c_char) -> libc::size_t;
}

#[derive(Clone)]
pub struct htp_mpartp_t {
    pub multipart: htp_multipart_t,
    pub cfg: *mut htp_config::htp_cfg_t,
    pub extract_files: i32,
    pub extract_limit: i32,
    pub extract_dir: *mut i8,
    pub file_count: i32,

    // Parsing callbacks
    pub handle_data: Option<
        unsafe extern "C" fn(_: *mut htp_mpartp_t, _: *const u8, _: usize, _: i32) -> Status,
    >,
    pub handle_boundary: Option<unsafe extern "C" fn(_: *mut htp_mpartp_t) -> Status>,
    // Internal parsing fields; move into a private structure
    /// Parser state; one of MULTIPART_STATE_* constants.
    parser_state: htp_multipart_state_t,

    /// Keeps track of the current position in the boundary matching progress.
    /// When this field reaches boundary_len, we have a boundary match.
    pub boundary_match_pos: usize,

    /// Pointer to the part that is currently being processed.
    pub current_part: *mut htp_multipart_part_t,

    /// This parser consists of two layers: the outer layer is charged with
    /// finding parts, and the internal layer handles part data. There is an
    /// interesting interaction between the two parsers. Because the
    /// outer layer is seeing every line (it has to, in order to test for
    /// boundaries), it also effectively also splits input into lines. The
    /// inner parser deals with two areas: first is the headers, which are
    /// line based, followed by binary data. When parsing headers, the inner
    /// parser can reuse the lines identified by the outer parser. In this
    /// variable we keep the current parsing mode of the part, which helps
    /// us process input data more efficiently. The possible values are
    /// MULTIPART_MODE_LINE and MULTIPART_MODE_DATA.
    current_part_mode: htp_part_mode_t,

    /// Used for buffering when a potential boundary is fragmented
    /// across many input data buffers. On a match, the data stored here is
    /// discarded. When there is no match, the buffer is processed as data
    /// (belonging to the currently active part).
    pub boundary_pieces: *mut bstr_builder::bstr_builder_t,
    pub part_header_pieces: *mut bstr_builder::bstr_builder_t,
    pub pending_header_line: *mut bstr::bstr_t,

    /// Stores text part pieces until the entire part is seen, at which
    /// point the pieces are assembled into a single buffer, and the
    /// builder cleared.
    pub part_data_pieces: *mut bstr_builder::bstr_builder_t,

    /// The offset of the current boundary candidate, relative to the most
    /// recent data chunk (first unprocessed chunk of data).
    pub boundary_candidate_pos: usize,

    /// When we encounter a CR as the last byte in a buffer, we don't know
    /// if the byte is part of a CRLF combination. If it is, then the CR
    /// might be a part of a boundary. But if it is not, it's current
    /// part's data. Because we know how to handle everything before the
    /// CR, we do, and we use this flag to indicate that a CR byte is
    /// effectively being buffered. This is probably a case of premature
    /// optimization, but I am going to leave it in for now.
    pub cr_aside: i32,

    /// When set, indicates that this parser no longer owns names and
    /// values of MULTIPART_PART_TEXT parts. It is used to avoid data
    /// duplication when the parser is used by LibHTP internally.
    pub gave_up_data: i32,
}

/// Holds information related to a part.
#[derive(Clone)]
pub struct htp_multipart_part_t {
    /// Pointer to the parser.
    pub parser: *mut htp_mpartp_t,
    /// Part type; see the MULTIPART_PART_* constants.
    pub type_0: htp_multipart_type_t,
    /// Raw part length (i.e., headers and data).
    pub len: usize,
    /// Part name, from the Content-Disposition header. Can be NULL.
    pub name: *mut bstr::bstr_t,

    /// Part value; the contents depends on the type of the part:
    /// 1) NULL for files; 2) contains complete part contents for
    /// preamble and epilogue parts (they have no headers), and
    /// 3) data only (headers excluded) for text and unknown parts.
    pub value: *mut bstr::bstr_t,
    /// Part content type, from the Content-Type header. Can be NULL.
    pub content_type: *mut bstr::bstr_t,
    /// Part headers (htp_header_t instances), using header name as the key.
    pub headers: htp_transaction::htp_headers_t,
    /// File data, available only for MULTIPART_PART_FILE parts.
    pub file: *mut htp_util::htp_file_t,
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Debug)]
enum htp_part_mode_t {
    /// When in line mode, the parser is handling part headers.
    MODE_LINE,
    /// When in data mode, the parser is consuming part data.
    MODE_DATA,
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Debug)]
enum htp_multipart_state_t {
    /// Initial state, after the parser has been created but before the boundary initialized.
    STATE_INIT,
    /// Processing data, waiting for a new line (which might indicate a new boundary).
    STATE_DATA,
    /// Testing a potential boundary.
    STATE_BOUNDARY,
    /// Checking the first byte after a boundary.
    STATE_BOUNDARY_IS_LAST1,
    /// Checking the second byte after a boundary.
    STATE_BOUNDARY_IS_LAST2,
    /// Consuming linear whitespace after a boundary.
    STATE_BOUNDARY_EAT_LWS,
    /// Used after a CR byte is detected in htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS.
    STATE_BOUNDARY_EAT_LWS_CR,
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum htp_multipart_type_t {
    /// Unknown part.
    MULTIPART_PART_UNKNOWN,
    /// Text (parameter) part.
    MULTIPART_PART_TEXT,
    /// File part.
    MULTIPART_PART_FILE,
    /// Free-text part before the first boundary.
    MULTIPART_PART_PREAMBLE,
    /// Free-text part after the last boundary.
    MULTIPART_PART_EPILOGUE,
}

/// Holds information related to a multipart body.
#[derive(Clone)]
pub struct htp_multipart_t {
    /// Multipart boundary.
    pub boundary: *mut i8,
    /// Boundary length.
    pub boundary_len: usize,
    /// How many boundaries were there?
    pub boundary_count: i32,
    /// List of parts, in the order in which they appeared in the body.
    pub parts: list::List<*mut htp_multipart_part_t>,
    /// Parsing flags.
    pub flags: MultipartFlags,
}

/// Returns the multipart structure created by the parser.
///
/// Returns The main multipart structure.
pub unsafe extern "C" fn htp_mpartp_get_multipart(
    parser: *mut htp_mpartp_t,
) -> *mut htp_multipart_t {
    &mut (*parser).multipart
}

/// Extracts and decodes a C-D header param name and value following a form-data. This is impossible to do correctly without a
/// parsing personality because most browsers are broken:
///  - Firefox encodes " as \", and \ is not encoded.
///  - Chrome encodes " as %22.
///  - IE encodes " as \", and \ is not encoded.
///  - Opera encodes " as \" and \ as \\.
fn content_disposition_param() -> impl Fn(&[u8]) -> IResult<&[u8], (&[u8], Vec<u8>)> {
    move |input| {
        let (mut remaining_input, param_name) = map(
            tuple((
                take_ascii_whitespace(),
                char(';'),
                take_ascii_whitespace(),
                take_while(|c: u8| c != '=' as u8 && !c.is_ascii_whitespace()),
                take_ascii_whitespace(),
                char('='),
                take_ascii_whitespace(),
                char('\"'), //must start with opening quote
            )),
            |(_, _, _, param_name, _, _, _, _)| param_name,
        )(input)?;
        // Unescape any escaped " and \ and find the closing "
        let mut param_value = Vec::new();
        loop {
            let (left, (value, to_insert)) = tuple((
                take_while(|c: u8| c != '\"' as u8 && c != '\\' as u8),
                opt(tuple((char('\\'), alt((char('\"'), char('\\')))))),
            ))(remaining_input)?;
            remaining_input = left;
            param_value.extend_from_slice(value);
            if let Some((_, to_insert)) = to_insert {
                // Insert the character
                param_value.push(to_insert as u8);
            } else {
                // Must end with a quote or it is invalid
                let (left, _) = char('\"')(remaining_input)?;
                remaining_input = left;
                break;
            }
        }
        Ok((remaining_input, (param_name, param_value)))
    }
}

/// Extracts and decodes a C-D header param names and values. This is impossible to do correctly without a
/// parsing personality because most browsers are broken:
///  - Firefox encodes " as \", and \ is not encoded.
///  - Chrome encodes " as %22.
///  - IE encodes " as \", and \ is not encoded.
///  - Opera encodes " as \" and \ as \\.
fn content_disposition<'a>(input: &'a [u8]) -> IResult<&'a [u8], Vec<(&'a [u8], Vec<u8>)>> {
    // Multiple header values are seperated by a ", ": https://tools.ietf.org/html/rfc7230#section-3.2.2
    map(
        tuple((
            tag("form-data"),
            fold_many1(
                tuple((
                    content_disposition_param(),
                    take_ascii_whitespace(),
                    opt(tuple((tag(","), take_ascii_whitespace(), tag("form-data")))),
                    take_ascii_whitespace(),
                )),
                Vec::new(),
                |mut acc: Vec<(&'a [u8], Vec<u8>)>, (param, _, _, _)| {
                    acc.push(param);
                    acc
                },
            ),
            take_ascii_whitespace(),
            opt(tag(";")), // Allow trailing semicolon,
            take_ascii_whitespace(),
            not(take(1usize)), // We should have no data left, or we exited parsing prematurely
        )),
        |(_, result, _, _, _, _)| result,
    )(input)
}

/// Parses the Content-Disposition part header.
///
/// Returns HTP_OK on success (header found and parsed), HTP_DECLINED if there is no C-D header or if
///         it could not be processed, and HTP_ERROR on fatal error.
pub unsafe fn htp_mpart_part_parse_c_d(part: *mut htp_multipart_part_t) -> Status {
    // Find the C-D header.
    let header = {
        if let Some((_, header)) = (*part).headers.get_nocase_nozero_mut("content-disposition") {
            header
        } else {
            (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_UNKNOWN;
            return Status::DECLINED;
        }
    };

    // Require "form-data" at the beginning of the header.
    if let Ok((_, params)) = content_disposition((*header.value).as_slice()) {
        for (param_name, param_value) in params {
            match param_name {
                b"name" => {
                    // If we've reached the end of the string that means the
                    // value was not terminated properly (the second double quote is missing).
                    // Expecting the terminating double quote.
                    // Over the terminating double quote.
                    // Finally, process the parameter value.
                    // Check that we have not seen the name parameter already.
                    if !(*part).name.is_null() {
                        (*(*part).parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_CD_PARAM_REPEATED;
                        return Status::DECLINED;
                    }
                    (*part).name = bstr::bstr_dup_str(param_value);
                    if (*part).name.is_null() {
                        return Status::ERROR;
                    }
                }
                b"filename" => {
                    // Check that we have not seen the filename parameter already.
                    if !(*part).file.is_null() {
                        (*(*part).parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_CD_PARAM_REPEATED;
                        return Status::DECLINED;
                    }
                    (*part).file = calloc(1, ::std::mem::size_of::<htp_util::htp_file_t>())
                        as *mut htp_util::htp_file_t;
                    if (*part).file.is_null() {
                        return Status::ERROR;
                    }
                    (*(*part).file).fd = -1;
                    (*(*part).file).source = htp_util::htp_file_source_t::HTP_FILE_MULTIPART;
                    (*(*part).file).filename = bstr::bstr_dup_str(param_value);
                    if (*(*part).file).filename.is_null() {
                        free((*part).file as *mut core::ffi::c_void);
                        return Status::ERROR;
                    }
                }
                _ => {
                    // Unknown parameter.
                    (*(*part).parser).multipart.flags |=
                        MultipartFlags::HTP_MULTIPART_CD_PARAM_UNKNOWN;
                    return Status::DECLINED;
                }
            }
        }
    } else {
        (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_CD_SYNTAX_INVALID;
        return Status::DECLINED;
    }
    Status::OK
}

/// Parses the Content-Type part header, if present.
///
/// Returns HTP_OK on success, HTP_DECLINED if the C-T header is not present, and HTP_ERROR on failure.
unsafe extern "C" fn htp_mpart_part_parse_c_t(part: *mut htp_multipart_part_t) -> Status {
    if let Some((_, header)) = (*part).headers.get_nocase_nozero("content-type") {
        if (*part).content_type.is_null() {
            (*part).content_type = bstr::bstr_alloc(0);
            if (*part).content_type.is_null() {
                return Status::ERROR;
            }
        }
        htp_util::htp_parse_ct_header(&header.value, &mut *(*part).content_type)
    } else {
        Status::DECLINED
    }
}

/// Processes part headers.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
pub unsafe extern "C" fn htp_mpart_part_process_headers(part: *mut htp_multipart_part_t) -> Status {
    if htp_mpart_part_parse_c_d(part) == Status::ERROR {
        return Status::ERROR;
    }
    if htp_mpart_part_parse_c_t(part) == Status::ERROR {
        return Status::ERROR;
    }
    Status::OK
}
/// Parses header, extracting a valid name and valid value.
/// Does not allow leading or trailing whitespace for a name, but allows leading and trailing whitespace for the value.
///
/// Returns a tuple of a valid name and value
fn header<'a>() -> impl Fn(&'a [u8]) -> IResult<&'a [u8], (&'a [u8], &'a [u8])> {
    move |input| {
        let (value, (name, _, _, _)) = tuple((
            // The name must not be empty and must consist only of token characters (i.e., no spaces, seperators, control characters, etc)
            take_while1(|c: u8| htp_util::htp_is_token(c)),
            // First non token character must be a colon, to seperate name and value
            tag(":"),
            // Allow whitespace between the colon and the value
            take_while(|c| is_space(c)),
            // Peek ahead to ensure a non empty header value
            peek(take(1usize)),
        ))(input)?;
        Ok((b"", (name, value)))
    }
}

/// Parses one part header.
///
/// Returns HTP_OK on success, HTP_DECLINED on parsing error, HTP_ERROR on fatal error.
pub unsafe fn htp_mpartp_parse_header<'a>(
    part: *mut htp_multipart_part_t,
    input: &'a [u8],
) -> Status {
    // We do not allow NUL bytes here.
    if input.contains(&('\0' as u8)) {
        (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_NUL_BYTE;
        return Status::DECLINED;
    }
    // Extract the name and the value
    if let Ok((_, (name, value))) = header()(input) {
        // Now extract the name and the value.
        let header = htp_transaction::htp_header_t::new(name.into(), value.into());

        if header.name.cmp_nocase("content-disposition") != Ordering::Equal
            && header.name.cmp_nocase("content-type") != Ordering::Equal
        {
            (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_HEADER_UNKNOWN
        }
        // Check if the header already exists.
        if let Some((_, h_existing)) = (*part).headers.get_nocase_mut(header.name.as_slice()) {
            h_existing.value.extend_from_slice(b", ");
            h_existing.value.extend_from_slice(header.value.as_slice());
            // Keep track of same-name headers.
            // FIXME: Normalize the flags? define the symbol in both Flags and MultipartFlags and set the value in both from their own namespace
            h_existing.flags |= Flags::from_bits_unchecked(
                MultipartFlags::HTP_MULTIPART_PART_HEADER_REPEATED.bits(),
            );
            (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_HEADER_REPEATED
        } else {
            (*part).headers.add(header.name.clone(), header);
        }
    } else {
        // Invalid name and/or value found
        (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_HEADER_INVALID;
        return Status::DECLINED;
    }
    Status::OK
}

/// Creates a new Multipart part.
///
/// Returns New part instance, or NULL on memory allocation failure.
pub unsafe extern "C" fn htp_mpart_part_create(
    parser: *mut htp_mpartp_t,
) -> *mut htp_multipart_part_t {
    let mut part: *mut htp_multipart_part_t =
        calloc(1, ::std::mem::size_of::<htp_multipart_part_t>()) as *mut htp_multipart_part_t;
    if part.is_null() {
        return 0 as *mut htp_multipart_part_t;
    }
    (*part).headers = htp_table::htp_table_t::with_capacity(4);
    (*part).parser = parser;
    bstr_builder::bstr_builder_clear((*parser).part_data_pieces);
    bstr_builder::bstr_builder_clear((*parser).part_header_pieces);
    part
}

/// Destroys a part.
pub unsafe extern "C" fn htp_mpart_part_destroy(
    mut part: *mut htp_multipart_part_t,
    gave_up_data: i32,
) {
    if part.is_null() {
        return;
    }
    if !(*part).file.is_null() {
        bstr::bstr_free((*(*part).file).filename);
        if !(*(*part).file).tmpname.is_null() {
            unlink((*(*part).file).tmpname);
            free((*(*part).file).tmpname as *mut core::ffi::c_void);
        }
        free((*part).file as *mut core::ffi::c_void);
        (*part).file = 0 as *mut htp_util::htp_file_t
    }
    if gave_up_data == 0 || (*part).type_0 != htp_multipart_type_t::MULTIPART_PART_TEXT {
        bstr::bstr_free((*part).name);
        bstr::bstr_free((*part).value);
    }
    bstr::bstr_free((*part).content_type);
    (*part).headers.elements.clear();
    free(part as *mut core::ffi::c_void);
}

/// Finalizes part processing.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
pub unsafe extern "C" fn htp_mpart_part_finalize_data(
    mut part: *mut htp_multipart_part_t,
) -> Status {
    // Determine if this part is the epilogue.
    if (*(*part).parser)
        .multipart
        .flags
        .contains(MultipartFlags::HTP_MULTIPART_SEEN_LAST_BOUNDARY)
    {
        if (*part).type_0 == htp_multipart_type_t::MULTIPART_PART_UNKNOWN {
            // Assume that the unknown part after the last boundary is the epilogue.
            (*(*(*part).parser).current_part).type_0 =
                htp_multipart_type_t::MULTIPART_PART_EPILOGUE;
            // But if we've already seen a part we thought was the epilogue,
            // raise HTP_MULTIPART_PART_UNKNOWN. Multiple epilogues are not allowed.
            if (*(*part).parser)
                .multipart
                .flags
                .contains(MultipartFlags::HTP_MULTIPART_HAS_EPILOGUE)
            {
                (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_UNKNOWN
            }
            (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_HAS_EPILOGUE
        } else {
            (*(*part).parser).multipart.flags |=
                MultipartFlags::HTP_MULTIPART_PART_AFTER_LAST_BOUNDARY
        }
    }
    // Sanity checks.
    // Have we seen complete part headers? If we have not, that means that the part ended prematurely.
    if (*(*(*part).parser).current_part).type_0 != htp_multipart_type_t::MULTIPART_PART_EPILOGUE
        && (*(*part).parser).current_part_mode != htp_part_mode_t::MODE_DATA
    {
        (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_INCOMPLETE
    }
    // Have we been able to determine the part type? If not, this means
    // that the part did not contain the C-D header.
    if (*part).type_0 == htp_multipart_type_t::MULTIPART_PART_UNKNOWN {
        (*(*part).parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_PART_UNKNOWN
    }
    // Finalize part value.
    if (*part).type_0 == htp_multipart_type_t::MULTIPART_PART_FILE {
        // Notify callbacks about the end of the file.
        htp_mpartp_run_request_file_data_hook(part, 0 as *const u8, 0);
        // If we are storing the file to disk, close the file descriptor.
        if (*(*part).file).fd != -1 {
            close((*(*part).file).fd);
        }
    } else if bstr_builder::bstr_builder_size((*(*part).parser).part_data_pieces) > 0 {
        (*part).value = bstr_builder::bstr_builder_to_str((*(*part).parser).part_data_pieces);
        bstr_builder::bstr_builder_clear((*(*part).parser).part_data_pieces);
    }
    Status::OK
}

pub unsafe extern "C" fn htp_mpartp_run_request_file_data_hook(
    mut part: *mut htp_multipart_part_t,
    data: *const u8,
    len: usize,
) -> Status {
    if (*(*part).parser).cfg.is_null() {
        return Status::OK;
    }
    // Combine value pieces into a single buffer.
    // Keep track of the file length.
    (*(*part).file).len = ((*(*part).file).len as u64).wrapping_add(len as u64) as i64;
    // Package data for the callbacks.
    let mut file_data: htp_util::htp_file_data_t = htp_util::htp_file_data_t {
        file: 0 as *mut htp_util::htp_file_t,
        data: 0 as *const u8,
        len: 0,
    };
    file_data.file = (*part).file;
    file_data.data = data;
    file_data.len = len;
    // Send data to callbacks
    let rc: Status = htp_hooks::htp_hook_run_all(
        (*(*(*part).parser).cfg).hook_request_file_data,
        &mut file_data as *mut htp_util::htp_file_data_t as *mut core::ffi::c_void,
    );
    if rc != Status::OK {
        return rc;
    }
    Status::OK
}

/// Handles part data.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
pub unsafe extern "C" fn htp_mpart_part_handle_data(
    mut part: *mut htp_multipart_part_t,
    mut data: *const u8,
    mut len: usize,
    is_line: i32,
) -> Status {
    // Keep track of raw part length.
    (*part).len = ((*part).len).wrapping_add(len);
    // If we're processing a part that came after the last boundary, then we're not sure if it
    // is the epilogue part or some other part (in case of evasion attempt). For that reason we
    // will keep all its data in the part_data_pieces structure. If it ends up not being the
    // epilogue, this structure will be cleared.
    if (*(*part).parser)
        .multipart
        .flags
        .contains(MultipartFlags::HTP_MULTIPART_SEEN_LAST_BOUNDARY)
        && (*part).type_0 == htp_multipart_type_t::MULTIPART_PART_UNKNOWN
    {
        bstr_builder::bstr_builder_append_mem(
            (*(*part).parser).part_data_pieces,
            data as *const core::ffi::c_void,
            len,
        );
    }
    if (*(*part).parser).current_part_mode == htp_part_mode_t::MODE_LINE {
        // Line mode.
        if is_line != 0 {
            // End of the line.
            let mut line: *mut bstr::bstr_t = 0 as *mut bstr::bstr_t;
            // If this line came to us in pieces, combine them now into a single buffer.
            if bstr_builder::bstr_builder_size((*(*part).parser).part_header_pieces) > 0 {
                bstr_builder::bstr_builder_append_mem(
                    (*(*part).parser).part_header_pieces,
                    data as *const core::ffi::c_void,
                    len,
                );
                line = bstr_builder::bstr_builder_to_str((*(*part).parser).part_header_pieces);
                if line.is_null() {
                    return Status::ERROR;
                }
                bstr_builder::bstr_builder_clear((*(*part).parser).part_header_pieces);
                data = bstr_ptr(line);
                len = bstr_len(line)
            }
            // Ignore the line endings.
            if len > 1 {
                if *data.offset(len.wrapping_sub(1) as isize) == '\n' as u8 {
                    len = len.wrapping_sub(1)
                }
                if *data.offset(len.wrapping_sub(1) as isize) == '\r' as u8 {
                    len = len.wrapping_sub(1)
                }
            } else if len > 0 && *data.offset(len.wrapping_sub(1) as isize) == '\n' as u8 {
                len = len.wrapping_sub(1)
            }
            // Is it an empty line?
            if len == 0 {
                // Empty line; process headers and switch to data mode.
                // Process the pending header, if any.
                if !(*(*part).parser).pending_header_line.is_null() {
                    if htp_mpartp_parse_header(
                        part,
                        &(*(*(*part).parser).pending_header_line).as_slice(),
                    ) == Status::ERROR
                    {
                        bstr::bstr_free(line);
                        return Status::ERROR;
                    }
                    bstr::bstr_free((*(*part).parser).pending_header_line);
                    (*(*part).parser).pending_header_line = 0 as *mut bstr::bstr_t
                }
                if htp_mpart_part_process_headers(part) == Status::ERROR {
                    bstr::bstr_free(line);
                    return Status::ERROR;
                }
                (*(*part).parser).current_part_mode = htp_part_mode_t::MODE_DATA;
                bstr_builder::bstr_builder_clear((*(*part).parser).part_header_pieces);
                if !(*part).file.is_null() {
                    // Changing part type because we have a filename.
                    (*part).type_0 = htp_multipart_type_t::MULTIPART_PART_FILE;
                    if (*(*part).parser).extract_files != 0
                        && (*(*part).parser).file_count < (*(*part).parser).extract_limit
                    {
                        let mut buf: [i8; 255] = [0; 255];
                        strncpy(buf.as_mut_ptr(), (*(*part).parser).extract_dir, 254);
                        strncat(
                            buf.as_mut_ptr(),
                            b"/libhtp-multipart-file-XXXXXX\x00" as *const u8 as *const i8,
                            (254 as usize).wrapping_sub(strlen(buf.as_mut_ptr())),
                        );
                        (*(*part).file).tmpname = strdup(buf.as_mut_ptr());
                        if (*(*part).file).tmpname.is_null() {
                            bstr::bstr_free(line);
                            return Status::ERROR;
                        }
                        let previous_mask: u32 = umask(
                            (0o100 as i32
                                | (0o400 as i32 | 0o200 as i32 | 0o100 as i32) >> 3 as i32
                                | (0o400 as i32 | 0o200 as i32 | 0o100 as i32)
                                    >> 3 as i32
                                    >> 3 as i32) as u32,
                        );
                        (*(*part).file).fd = mkstemp((*(*part).file).tmpname);
                        umask(previous_mask);
                        if (*(*part).file).fd < 0 {
                            bstr::bstr_free(line);
                            return Status::ERROR;
                        }
                        (*(*part).parser).file_count += 1
                    }
                } else if !(*part).name.is_null() {
                    // Changing part type because we have a name.
                    (*part).type_0 = htp_multipart_type_t::MULTIPART_PART_TEXT;
                    bstr_builder::bstr_builder_clear((*(*part).parser).part_data_pieces);
                }
            } else if (*(*part).parser).pending_header_line.is_null() {
                if !line.is_null() {
                    (*(*part).parser).pending_header_line = line;
                    line = 0 as *mut bstr::bstr_t
                } else {
                    (*(*part).parser).pending_header_line =
                        bstr::bstr_dup_mem(data as *const core::ffi::c_void, len);
                    if (*(*part).parser).pending_header_line.is_null() {
                        return Status::ERROR;
                    }
                }
            } else if (*data.offset(0)).is_ascii_whitespace() {
                // Not an empty line.
                // Is there a pending header?
                // Is this a folded line?
                // Folding; add to the existing line.
                (*(*part).parser).multipart.flags |=
                    MultipartFlags::HTP_MULTIPART_PART_HEADER_FOLDING;
                (*(*part).parser).pending_header_line = bstr::bstr_add_mem(
                    (*(*part).parser).pending_header_line,
                    data as *const core::ffi::c_void,
                    len,
                );
                if (*(*part).parser).pending_header_line.is_null() {
                    bstr::bstr_free(line);
                    return Status::ERROR;
                }
            } else {
                // Process the pending header line.
                if htp_mpartp_parse_header(
                    part,
                    &(*(*(*part).parser).pending_header_line).as_slice(),
                ) == Status::ERROR
                {
                    bstr::bstr_free(line);
                    return Status::ERROR;
                }
                bstr::bstr_free((*(*part).parser).pending_header_line);
                if !line.is_null() {
                    (*(*part).parser).pending_header_line = line;
                    line = 0 as *mut bstr::bstr_t
                } else {
                    (*(*part).parser).pending_header_line =
                        bstr::bstr_dup_mem(data as *const core::ffi::c_void, len);
                    if (*(*part).parser).pending_header_line.is_null() {
                        return Status::ERROR;
                    }
                }
            }
            bstr::bstr_free(line);
            line = 0 as *mut bstr::bstr_t
        } else {
            // Not end of line; keep the data chunk for later.
            bstr_builder::bstr_builder_append_mem(
                (*(*part).parser).part_header_pieces,
                data as *const core::ffi::c_void,
                len,
            );
        }
    } else {
        // Data mode; keep the data chunk for later (but not if it is a file).
        match (*part).type_0 as u32 {
            4 | 3 | 1 | 0 => {
                // Make a copy of the data in RAM.
                bstr_builder::bstr_builder_append_mem(
                    (*(*part).parser).part_data_pieces,
                    data as *const core::ffi::c_void,
                    len,
                );
            }
            2 => {
                // Invoke file data callbacks.
                htp_mpartp_run_request_file_data_hook(part, data, len);
                // Optionally, store the data in a file.
                if (*(*part).file).fd != -1
                    && write((*(*part).file).fd, data as *const core::ffi::c_void, len) < 0
                {
                    return Status::ERROR;
                }
            }
            _ => {
                // Internal error.
                return Status::ERROR;
            }
        }
    }
    Status::OK
}

/// Handles data, creating new parts as necessary.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
unsafe extern "C" fn htp_mpartp_handle_data(
    mut parser: *mut htp_mpartp_t,
    data: *const u8,
    len: usize,
    is_line: i32,
) -> Status {
    if len == 0 {
        return Status::OK;
    }
    // Do we have a part already?
    if (*parser).current_part.is_null() {
        // Create a new part.
        (*parser).current_part = htp_mpart_part_create(parser);
        if (*parser).current_part.is_null() {
            return Status::ERROR;
        }
        if (*parser).multipart.boundary_count == 0 {
            // We haven't seen a boundary yet, so this must be the preamble part.
            (*(*parser).current_part).type_0 = htp_multipart_type_t::MULTIPART_PART_PREAMBLE;
            (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_HAS_PREAMBLE;
            (*parser).current_part_mode = htp_part_mode_t::MODE_DATA
        } else {
            // Part after preamble.
            (*parser).current_part_mode = htp_part_mode_t::MODE_LINE
        }
        // Add part to the list.
        (*parser).multipart.parts.push((*parser).current_part);
    }
    // Send data to the part.
    htp_mpart_part_handle_data((*parser).current_part, data, len, is_line)
}

/// Handles a boundary event, which means that it will finalize a part if one exists.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
unsafe extern "C" fn htp_mpartp_handle_boundary(mut parser: *mut htp_mpartp_t) -> Status {
    if !(*parser).current_part.is_null() {
        if htp_mpart_part_finalize_data((*parser).current_part) != Status::OK {
            return Status::OK;
        }
        // We're done with this part
        (*parser).current_part = 0 as *mut htp_multipart_part_t;
        // Revert to line mode
        (*parser).current_part_mode = htp_part_mode_t::MODE_LINE
    }
    Status::OK
}

unsafe extern "C" fn htp_mpartp_init_boundary(
    mut parser: *mut htp_mpartp_t,
    data: *const u8,
    len: usize,
) -> Status {
    if parser.is_null() || data.is_null() {
        return Status::ERROR;
    }
    // Copy the boundary and convert it to lowercase.
    (*parser).multipart.boundary_len = len.wrapping_add(4);
    (*parser).multipart.boundary =
        malloc((*parser).multipart.boundary_len.wrapping_add(1)) as *mut i8;
    if (*parser).multipart.boundary.is_null() {
        return Status::ERROR;
    }
    *(*parser).multipart.boundary.offset(0) = '\r' as i8;
    *(*parser).multipart.boundary.offset(1) = '\n' as i8;
    *(*parser).multipart.boundary.offset(2) = '-' as i8;
    *(*parser).multipart.boundary.offset(3) = '-' as i8;
    let mut i: usize = 0;
    while i < len {
        *(*parser)
            .multipart
            .boundary
            .offset(i.wrapping_add(4) as isize) = *data.offset(i as isize) as i8;
        i = i.wrapping_add(1)
    }
    *(*parser)
        .multipart
        .boundary
        .offset((*parser).multipart.boundary_len as isize) = '\u{0}' as i8;
    // We're starting in boundary-matching mode. The first boundary can appear without the
    // CRLF, and our starting state expects that. If we encounter non-boundary data, the
    // state will switch to data mode. Then, if the data is CRLF or LF, we will go back
    // to boundary matching. Thus, we handle all the possibilities.
    (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY;
    (*parser).boundary_match_pos = 2;
    Status::OK
}

/// Creates a new multipart/form-data parser. On a successful invocation,
/// the ownership of the boundary parameter is transferred to the parser.
///
/// Returns New parser instance, or NULL on memory allocation failure.
pub unsafe extern "C" fn htp_mpartp_create(
    cfg: *mut htp_config::htp_cfg_t,
    boundary: *mut bstr::bstr_t,
    flags: MultipartFlags,
) -> *mut htp_mpartp_t {
    if cfg.is_null() || boundary.is_null() {
        return 0 as *mut htp_mpartp_t;
    }
    let mut parser: *mut htp_mpartp_t =
        calloc(1, ::std::mem::size_of::<htp_mpartp_t>()) as *mut htp_mpartp_t;
    if parser.is_null() {
        return 0 as *mut htp_mpartp_t;
    }
    (*parser).cfg = cfg;
    (*parser).boundary_pieces = bstr_builder::bstr_builder_create();
    if (*parser).boundary_pieces.is_null() {
        htp_mpartp_destroy(parser);
        return 0 as *mut htp_mpartp_t;
    }
    (*parser).part_data_pieces = bstr_builder::bstr_builder_create();
    if (*parser).part_data_pieces.is_null() {
        htp_mpartp_destroy(parser);
        return 0 as *mut htp_mpartp_t;
    }
    (*parser).part_header_pieces = bstr_builder::bstr_builder_create();
    if (*parser).part_header_pieces.is_null() {
        htp_mpartp_destroy(parser);
        return 0 as *mut htp_mpartp_t;
    }
    (*parser).multipart.parts = list::List::with_capacity(64);
    (*parser).multipart.flags = flags;
    (*parser).parser_state = htp_multipart_state_t::STATE_INIT;
    (*parser).extract_files = (*cfg).extract_request_files;
    (*parser).extract_dir = (*cfg).tmpdir;
    if (*cfg).extract_request_files_limit >= 0 {
        (*parser).extract_limit = (*cfg).extract_request_files_limit
    } else {
        (*parser).extract_limit = 16
    }
    (*parser).handle_data = Some(
        htp_mpartp_handle_data
            as unsafe extern "C" fn(_: *mut htp_mpartp_t, _: *const u8, _: usize, _: i32) -> Status,
    );
    (*parser).handle_boundary =
        Some(htp_mpartp_handle_boundary as unsafe extern "C" fn(_: *mut htp_mpartp_t) -> Status);
    // Initialize the boundary.
    let rc: Status = htp_mpartp_init_boundary(parser, bstr_ptr(boundary), bstr_len(boundary));
    if rc != Status::OK {
        htp_mpartp_destroy(parser);
        return 0 as *mut htp_mpartp_t;
    }
    // On success, the ownership of the boundary parameter
    // is transferred to us. We made a copy, and so we
    // don't need it any more.
    bstr::bstr_free(boundary);
    parser
}

/// Destroys the provided parser.
pub unsafe extern "C" fn htp_mpartp_destroy(parser: *mut htp_mpartp_t) {
    if parser.is_null() {
        return;
    }
    if !(*parser).multipart.boundary.is_null() {
        free((*parser).multipart.boundary as *mut core::ffi::c_void);
    }
    bstr_builder::bstr_builder_destroy((*parser).boundary_pieces);
    bstr_builder::bstr_builder_destroy((*parser).part_header_pieces);
    bstr::bstr_free((*parser).pending_header_line);
    bstr_builder::bstr_builder_destroy((*parser).part_data_pieces);
    // Free the parts.
    for part in &(*parser).multipart.parts {
        htp_mpart_part_destroy(*part, (*parser).gave_up_data);
    }
    drop(&(*parser).multipart.parts);
    free(parser as *mut core::ffi::c_void);
}

/// Processes set-aside data.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
unsafe extern "C" fn htp_martp_process_aside(
    mut parser: *mut htp_mpartp_t,
    matched: i32,
) -> Status {
    // The stored data pieces can contain up to one line. If we're in data mode and there
    // was no boundary match, things are straightforward -- we process everything as data.
    // If there was a match, we need to take care to not send the line ending as data, nor
    // anything that follows (because it's going to be a part of the boundary). Similarly,
    // when we are in line mode, we need to split the first data chunk, processing the first
    // part as line and the second part as data.
    // Do we need to do any chunk splitting?
    if matched != 0 || (*parser).current_part_mode == htp_part_mode_t::MODE_LINE {
        // Line mode or boundary match
        // Process the CR byte, if set aside.
        if matched == 0 && (*parser).cr_aside != 0 {
            // Treat as part data, when there is not a match.
            (*parser).handle_data.expect("non-null function pointer")(
                parser,
                &*::std::mem::transmute::<&[u8; 2], &[i8; 2]>(b"\r\x00") as *const [i8; 2]
                    as *mut u8,
                1,
                0,
            );
            (*parser).cr_aside = 0
        } else {
            // Treat as boundary, when there is a match.
            (*parser).cr_aside = 0
        }
        // We know that we went to match a boundary because
        // we saw a new line. Now we have to find that line and
        // process it. It's either going to be in the current chunk,
        // or in the first stored chunk.
        if bstr_builder::bstr_builder_size((*parser).boundary_pieces) > 0 {
            let mut first: i32 = 1;
            for b in &(*(*parser).boundary_pieces).pieces {
                if first != 0 {
                    first = 0;
                    // Split the first chunk.
                    if matched == 0 {
                        // In line mode, we are OK with line endings.
                        (*parser).handle_data.expect("non-null function pointer")(
                            parser,
                            bstr_ptr(*b),
                            (*parser).boundary_candidate_pos,
                            1,
                        );
                    } else {
                        // But if there was a match, the line ending belongs to the boundary.
                        let dx: *mut u8 = bstr_ptr(*b);
                        let mut lx: usize = (*parser).boundary_candidate_pos;
                        // Remove LF or CRLF.
                        if lx > 0 && *dx.offset(lx.wrapping_sub(1) as isize) == '\n' as u8 {
                            lx = lx.wrapping_sub(1);
                            // Remove CR.
                            if lx > 0 && *dx.offset(lx.wrapping_sub(1) as isize) == '\r' as u8 {
                                lx = lx.wrapping_sub(1)
                            }
                        }
                        (*parser).handle_data.expect("non-null function pointer")(
                            parser, dx, lx, 0,
                        );
                    }
                    // The second part of the split chunks belongs to the boundary
                    // when matched, data otherwise.
                    if matched == 0 {
                        (*parser).handle_data.expect("non-null function pointer")(
                            parser,
                            bstr_ptr(*b).offset((*parser).boundary_candidate_pos as isize),
                            bstr_len(*b).wrapping_sub((*parser).boundary_candidate_pos),
                            0,
                        );
                    }
                } else if matched == 0 {
                    (*parser).handle_data.expect("non-null function pointer")(
                        parser,
                        bstr_ptr(*b),
                        bstr_len(*b),
                        0,
                    );
                }
            }
            bstr_builder::bstr_builder_clear((*parser).boundary_pieces);
        }
    } else {
        // Do not send data if there was a boundary match. The stored
        // data belongs to the boundary.
        // Data mode and no match.
        // In data mode, we process the lone CR byte as data.
        if (*parser).cr_aside != 0 {
            (*parser).handle_data.expect("non-null function pointer")(
                parser,
                &*::std::mem::transmute::<&[u8; 2], &[i8; 2]>(b"\r\x00") as *const [i8; 2]
                    as *const u8,
                1,
                0,
            );
            (*parser).cr_aside = 0
        }
        // We then process any pieces that we might have stored, also as data.
        if bstr_builder::bstr_builder_size((*parser).boundary_pieces) > 0 {
            for each in &(*(*parser).boundary_pieces).pieces {
                (*parser).handle_data.expect("non-null function pointer")(
                    parser,
                    bstr_ptr(*each),
                    bstr_len(*each),
                    0,
                );
            }
            bstr_builder::bstr_builder_clear((*parser).boundary_pieces);
        }
    }
    Status::OK
}

/// Finalize parsing.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
pub unsafe extern "C" fn htp_mpartp_finalize(parser: *mut htp_mpartp_t) -> Status {
    if !(*parser).current_part.is_null() {
        // Process buffered data, if any.
        htp_martp_process_aside(parser, 0);
        // Finalize the last part.
        if htp_mpart_part_finalize_data((*parser).current_part) != Status::OK {
            return Status::ERROR;
        }
        // It is OK to end abruptly in the epilogue part, but not in any other.
        if (*(*parser).current_part).type_0 != htp_multipart_type_t::MULTIPART_PART_EPILOGUE {
            (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_INCOMPLETE
        }
    }
    bstr_builder::bstr_builder_clear((*parser).boundary_pieces);
    Status::OK
}

/// Parses a chunk of multipart/form-data data. This function should be called
/// as many times as necessary until all data has been consumed.
///
/// Returns HTP_OK on success, HTP_ERROR on failure.
pub unsafe extern "C" fn htp_mpartp_parse(
    mut parser: *mut htp_mpartp_t,
    _data: *const core::ffi::c_void,
    len: usize,
) -> Status {
    let data: *const u8 = _data as *const u8;
    // The current position in the entire input buffer.
    let mut pos: usize = 0;
    // The position of the first unprocessed byte of data. We split the
    // input buffer into smaller chunks, according to their purpose. Once
    // an entire such smaller chunk is processed, we move to the next
    // and update startpos.
    let mut startpos: usize = 0;
    // The position of the (possible) boundary. We investigate for possible
    // boundaries whenever we encounter CRLF or just LF. If we don't find a
    // boundary we need to go back, and this is what data_return_pos helps with.
    let mut data_return_pos: usize = 0;
    // While there's data in the input buffer.
    while pos < len {
        'c_11171: loop {
            match (*parser).parser_state as u32 {
                0 => {
                    // Incomplete initialization.
                    return Status::ERROR;
                }
                1 => {
                    // Handle part data.
                    // While there's data in the input buffer.
                    while pos < len {
                        // Check for a CRLF-terminated line.
                        if *data.offset(pos as isize) == '\r' as u8 {
                            // We have a CR byte.
                            // Is this CR the last byte in the input buffer?
                            if pos.wrapping_add(1) == len {
                                // We have CR as the last byte in input. We are going to process
                                // what we have in the buffer as data, except for the CR byte,
                                // which we're going to leave for later. If it happens that a
                                // CR is followed by a LF and then a boundary, the CR is going
                                // to be discarded.
                                pos = pos.wrapping_add(1); // Advance over CR.
                                (*parser).cr_aside = 1
                            } else if *data.offset(pos.wrapping_add(1) as isize) == '\n' as u8 {
                                // We have CR and at least one more byte in the buffer, so we
                                // are able to test for the LF byte too.
                                pos = (pos).wrapping_add(2); // Advance over CR and LF.
                                (*parser).multipart.flags |=
                                    MultipartFlags::HTP_MULTIPART_CRLF_LINE;
                                // Prepare to switch to boundary testing.
                                data_return_pos = pos; // After LF; position of the first dash.
                                (*parser).boundary_candidate_pos = pos.wrapping_sub(startpos);
                                (*parser).boundary_match_pos = 2;
                                (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY;
                                continue 'c_11171;
                            } else {
                                // This is not a new line; advance over the
                                // byte and clear the CR set-aside flag.
                                pos = pos.wrapping_add(1);
                                (*parser).cr_aside = 0
                            }
                        } else if *data.offset(pos as isize) == '\n' as u8 {
                            // Check for a LF-terminated line.
                            pos = pos.wrapping_add(1); // Advance over LF.
                                                       // Did we have a CR in the previous input chunk?
                            if (*parser).cr_aside == 0 {
                                (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_LF_LINE
                            } else {
                                (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_CRLF_LINE
                            }
                            // Prepare to switch to boundary testing.
                            data_return_pos = pos; // After LF; position of the first dash.
                            (*parser).boundary_candidate_pos = pos.wrapping_sub(startpos);
                            (*parser).boundary_match_pos = 2;
                            (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY;
                            continue 'c_11171;
                        } else {
                            // Take one byte from input
                            pos = pos.wrapping_add(1);
                            // Earlier we might have set aside a CR byte not knowing if the next
                            // byte is a LF. Now we know that it is not, and so we can release the CR.
                            if (*parser).cr_aside != 0 {
                                (*parser).handle_data.expect("non-null function pointer")(
                                    parser,
                                    &*::std::mem::transmute::<&[u8; 2], &[i8; 2]>(b"\r\x00")
                                        as *const [i8; 2]
                                        as *mut u8,
                                    1,
                                    0,
                                );
                                (*parser).cr_aside = 0
                            }
                        }
                    }
                    // No more data in the input buffer; process the data chunk.
                    (*parser).handle_data.expect("non-null function pointer")(
                        parser,
                        data.offset(startpos as isize),
                        pos.wrapping_sub(startpos)
                            .wrapping_sub((*parser).cr_aside as usize),
                        0,
                    );
                    break;
                }
                2 => {
                    // Handle a possible boundary.
                    while pos < len {
                        // Check if the bytes match.
                        if !(*data.offset(pos as isize) as i32
                            == *(*parser)
                                .multipart
                                .boundary
                                .offset((*parser).boundary_match_pos as isize)
                                as i32)
                        {
                            // Boundary mismatch.
                            // Process stored (buffered) data.
                            htp_martp_process_aside(parser, 0);
                            // Return back where data parsing left off.
                            if (*parser).current_part_mode == htp_part_mode_t::MODE_LINE {
                                // In line mode, we process the line.
                                (*parser).handle_data.expect("non-null function pointer")(
                                    parser,
                                    data.offset(startpos as isize),
                                    data_return_pos.wrapping_sub(startpos),
                                    1,
                                );
                                startpos = data_return_pos
                            } else {
                                // In data mode, we go back where we left off.
                                pos = data_return_pos
                            }
                            (*parser).parser_state = htp_multipart_state_t::STATE_DATA;
                            continue 'c_11171;
                        } else {
                            // Consume one matched boundary byte
                            pos = pos.wrapping_add(1);
                            (*parser).boundary_match_pos =
                                (*parser).boundary_match_pos.wrapping_add(1);
                            // Have we seen all boundary bytes?
                            if !((*parser).boundary_match_pos == (*parser).multipart.boundary_len) {
                                continue;
                            }
                            // Boundary match!
                            // Process stored (buffered) data.
                            htp_martp_process_aside(parser, 1);
                            // Process data prior to the boundary in the current input buffer.
                            // Because we know this is the last chunk before boundary, we can
                            // remove the line endings.
                            let mut dlen: usize = data_return_pos.wrapping_sub(startpos);
                            if dlen > 0
                                && *data
                                    .offset(startpos.wrapping_add(dlen).wrapping_sub(1) as isize)
                                    == '\n' as u8
                            {
                                dlen = dlen.wrapping_sub(1)
                            }
                            if dlen > 0
                                && *data
                                    .offset(startpos.wrapping_add(dlen).wrapping_sub(1) as isize)
                                    == '\r' as u8
                            {
                                dlen = dlen.wrapping_sub(1)
                            }
                            (*parser).handle_data.expect("non-null function pointer")(
                                parser,
                                data.offset(startpos as isize),
                                dlen,
                                1,
                            );
                            // Keep track of how many boundaries we've seen.
                            (*parser).multipart.boundary_count += 1;
                            if (*parser)
                                .multipart
                                .flags
                                .contains(MultipartFlags::HTP_MULTIPART_SEEN_LAST_BOUNDARY)
                            {
                                (*parser).multipart.flags |=
                                    MultipartFlags::HTP_MULTIPART_PART_AFTER_LAST_BOUNDARY
                            }
                            // Run boundary match.
                            (*parser)
                                .handle_boundary
                                .expect("non-null function pointer")(
                                parser
                            );
                            // We now need to check if this is the last boundary in the payload
                            (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_IS_LAST2;
                            continue 'c_11171;
                        }
                    }
                    // No more data in the input buffer; store (buffer) the unprocessed
                    // part for later, for after we find out if this is a boundary.
                    bstr_builder::bstr_builder_append_mem(
                        (*parser).boundary_pieces,
                        data.offset(startpos as isize) as *const core::ffi::c_void,
                        len.wrapping_sub(startpos),
                    );
                    break;
                }
                4 => {
                    // Examine the first byte after the last boundary character. If it is
                    // a dash, then we maybe processing the last boundary in the payload. If
                    // it is not, move to eat all bytes until the end of the line.
                    if *data.offset(pos as isize) == '-' as u8 {
                        // Found one dash, now go to check the next position.
                        pos = pos.wrapping_add(1);
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_IS_LAST1
                    } else {
                        // This is not the last boundary. Change state but
                        // do not advance the position, allowing the next
                        // state to process the byte.
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS
                    }
                    break;
                }
                3 => {
                    // Examine the byte after the first dash; expected to be another dash.
                    // If not, eat all bytes until the end of the line.
                    if *data.offset(pos as isize) == '-' as u8 {
                        // This is indeed the last boundary in the payload.
                        pos = pos.wrapping_add(1);
                        (*parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_SEEN_LAST_BOUNDARY;
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS
                    } else {
                        // The second character is not a dash, and so this is not
                        // the final boundary. Raise the flag for the first dash,
                        // and change state to consume the rest of the boundary line.
                        (*parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_BBOUNDARY_NLWS_AFTER;
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS
                    }
                    break;
                }
                5 => {
                    if *data.offset(pos as isize) == '\r' as u8 {
                        // CR byte, which could indicate a CRLF line ending.
                        pos = pos.wrapping_add(1);
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS_CR
                    } else if *data.offset(pos as isize) == '\n' as u8 {
                        // LF line ending; we're done with boundary processing; data bytes follow.
                        pos = pos.wrapping_add(1);
                        startpos = pos;
                        (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_LF_LINE;
                        (*parser).parser_state = htp_multipart_state_t::STATE_DATA
                    } else if htp_util::htp_is_lws(*data.offset(pos as isize)) {
                        // Linear white space is allowed here.
                        (*parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_BBOUNDARY_LWS_AFTER;
                        pos = pos.wrapping_add(1)
                    } else {
                        // Unexpected byte; consume, but remain in the same state.
                        (*parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_BBOUNDARY_NLWS_AFTER;
                        pos = pos.wrapping_add(1)
                    }
                    break;
                }
                6 => {
                    if *data.offset(pos as isize) == '\n' as u8 {
                        // CRLF line ending; we're done with boundary processing; data bytes follow.
                        pos = pos.wrapping_add(1);
                        startpos = pos;
                        (*parser).multipart.flags |= MultipartFlags::HTP_MULTIPART_CRLF_LINE;
                        (*parser).parser_state = htp_multipart_state_t::STATE_DATA
                    } else {
                        // Not a line ending; start again, but do not process this byte.
                        (*parser).multipart.flags |=
                            MultipartFlags::HTP_MULTIPART_BBOUNDARY_NLWS_AFTER;
                        (*parser).parser_state = htp_multipart_state_t::STATE_BOUNDARY_EAT_LWS
                    }
                    break;
                }
                _ => {
                    break;
                    // switch
                }
            }
        }
    }
    Status::OK
}
/// Validates a multipart boundary according to RFC 1341:
///
///    The only mandatory parameter for the multipart  Content-Type
///    is  the  boundary  parameter,  which  consists  of  1  to 70
///    characters from a set of characters known to be very  robust
///    through  email  gateways,  and  NOT ending with white space.
///    (If a boundary appears to end with white  space,  the  white
///    space  must be presumed to have been added by a gateway, and
///    should  be  deleted.)   It  is  formally  specified  by  the
///    following BNF:
///
///    boundary := 0*69<bchars> bcharsnospace
///
///    bchars := bcharsnospace / " "
///
///    bcharsnospace :=    DIGIT / ALPHA / "'" / "(" / ")" / "+" / "_"
///                          / "," / "-" / "." / "/" / ":" / "=" / "?"
///
///    Chrome: Content-Type: multipart/form-data; boundary=----WebKitFormBoundaryT4AfwQCOgIxNVwlD
///    Firefox: Content-Type: multipart/form-data; boundary=---------------------------21071316483088
///    MSIE: Content-Type: multipart/form-data; boundary=---------------------------7dd13e11c0452
///    Opera: Content-Type: multipart/form-data; boundary=----------2JL5oh7QWEDwyBllIRc7fh
///    Safari: Content-Type: multipart/form-data; boundary=----WebKitFormBoundaryre6zL3b0BelnTY5S
///
/// Returns in flags the appropriate MultipartFlags
fn validate_boundary(boundary: &[u8], flags: &mut MultipartFlags) {
    // The RFC allows up to 70 characters. In real life,
    // boundaries tend to be shorter.
    if boundary.len() == 0 || boundary.len() > 70 {
        *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
    }
    // Check boundary characters. This check is stricter than the
    // RFC, which seems to allow many separator characters.
    for byte in boundary {
        if !byte.is_ascii_alphanumeric() && *byte != '-' as u8 {
            match *byte as char {
                '\'' | '(' | ')' | '+' | '_' | ',' | '.' | '/' | ':' | '=' | '?' => {
                    // These characters are allowed by the RFC, but not common.
                    *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL
                }
                _ => {
                    // Invalid character.
                    *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
                }
            }
        }
    }
}

/// Validates the content type by checking if there are multiple boundary occurrences or any occurrence contains uppercase characters
///
/// Returns in flags the appropriate MultipartFlags

fn validate_content_type<'a>(content_type: &'a [u8], flags: &mut MultipartFlags) {
    if let Ok((_, (f, _))) = fold_many1(
        tuple((
            htp_util::take_until_no_case(b"boundary"),
            tag_no_case("boundary"),
            take_until("="),
            tag("="),
        )),
        (MultipartFlags::empty(), false),
        |(mut flags, mut seen_prev): (MultipartFlags, bool),
         (_, boundary, _, _): (_, &[u8], _, _)| {
            for byte in boundary {
                if byte.is_ascii_uppercase() {
                    flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID;
                    break;
                }
            }
            if seen_prev {
                // Seen multiple boundaries
                flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
            }
            seen_prev = true;
            (flags, seen_prev)
        },
    )(content_type)
    {
        *flags |= f;
    } else {
        // There must be at least one occurrence!
        *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID;
    }
}

/// Attempts to locate and extract the boundary from an input slice, returning a tuple of the matched
/// boundary and any leading/trailing whitespace and non whitespace characters that might be relevant
fn boundary<'a>() -> impl Fn(
    &'a [u8],
) -> IResult<
    &'a [u8],
    (
        &'a [u8],
        &'a [u8],
        &'a [u8],
        Option<char>,
        &'a [u8],
        Option<char>,
        &'a [u8],
        &'a [u8],
    ),
> {
    move |input| {
        map(
            tuple((
                htp_util::take_until_no_case(b"boundary"),
                tag_no_case("boundary"),
                take_while(|c: u8| htp_util::htp_is_space(c)),
                take_until("="),
                tag("="),
                take_while(|c: u8| htp_util::htp_is_space(c)),
                peek(opt(char('\"'))),
                alt((
                    map(tuple((tag("\""), take_until("\""))), |(_, boundary)| {
                        boundary
                    }),
                    map(
                        tuple((
                            take_while(|c: u8| {
                                c != ',' as u8 && c != ';' as u8 && !htp_util::htp_is_space(c)
                            }),
                            opt(alt((char(','), char(';')))), //Skip the matched character if we matched one without hitting the end
                        )),
                        |(boundary, _)| boundary,
                    ),
                )),
                peek(opt(char('\"'))),
                take_while(|c: u8| htp_util::htp_is_space(c)),
                take_while(|c: u8| !htp_util::htp_is_space(c)),
            )),
            |(
                _,
                _,
                spaces_before_equal,
                chars_before_equal,
                _,
                spaces_after_equal,
                opening_quote,
                boundary,
                closing_quote,
                spaces_after_boundary,
                chars_after_boundary,
            )| {
                (
                    spaces_before_equal,
                    chars_before_equal,
                    spaces_after_equal,
                    opening_quote,
                    boundary,
                    closing_quote,
                    spaces_after_boundary,
                    chars_after_boundary,
                )
            },
        )(input)
    }
}

/// Looks for boundary in the supplied Content-Type request header.
///
/// Returns in multipart_flags: Multipart flags, which are not compatible from general LibHTP flags.
///
/// Returns boundary if found, None otherwise.
/// Flags may be set on even without successfully locating the boundary. For
/// example, if a boundary could not be extracted but there is indication that
/// one is present, HTP_MULTIPART_HBOUNDARY_INVALID will be set.
pub fn htp_mpartp_find_boundary<'a>(
    content_type: &'a [u8],
    flags: &mut MultipartFlags,
) -> Option<&'a [u8]> {
    // Our approach is to ignore the MIME type and instead just look for
    // the boundary. This approach is more reliable in the face of various
    // evasion techniques that focus on submitting invalid MIME types.
    // Reset flags.
    *flags = MultipartFlags::empty();
    // Correlate with the MIME type. This might be a tad too
    // sensitive because it may catch non-browser access with sloppy
    // implementations, but let's go with it for now.
    if !content_type.starts_with(b"multipart/form-data;") {
        *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
    }
    // Look for the boundary, case insensitive.
    if let Ok((
        _,
        (
            spaces_before_equal,
            chars_before_equal,
            spaces_after_equal,
            opening_quote,
            boundary,
            closing_quote,
            spaces_after_boundary,
            chars_after_boundary,
        ),
    )) = boundary()(content_type)
    {
        if spaces_before_equal.len() > 0
            || spaces_after_equal.len() > 0
            || opening_quote.is_some()
            || (chars_after_boundary.len() == 0 && spaces_after_boundary.len() > 0)
        {
            // It is unusual to see whitespace before and/or after the equals sign.
            // Unusual to have a quoted boundary
            // Unusual but allowed to have only whitespace after the boundary
            *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL
        }
        if chars_before_equal.len() > 0
            || (opening_quote.is_some() && !closing_quote.is_some())
            || (!opening_quote.is_some() && closing_quote.is_some())
            || chars_after_boundary.len() > 0
        {
            // Seeing a non-whitespace character before equal sign may indicate evasion
            // Having an opening quote, but no closing quote is invalid
            // Seeing any character after the boundary, other than whitespace is invalid
            *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
        }
        if boundary.len() == 0 {
            *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID;
            return None;
        }
        // Validate boundary characters.
        validate_boundary(boundary, flags);
        validate_content_type(content_type, flags);
        Some(boundary)
    } else {
        *flags |= MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID;
        None
    }
}

#[test]
fn Boundary() {
    let inputs: Vec<&[u8]> = vec![
        b"multipart/form-data; boundary=myboundarydata",
        b"multipart/form-data; BounDary=myboundarydata",
        b"multipart/form-data; boundary   =myboundarydata",
        b"multipart/form-data; boundary=   myboundarydata",
        b"multipart/form-data; boundary=myboundarydata ",
        b"multipart/form-data; boundary=myboundarydata, ",
        b"multipart/form-data; boundary=myboundarydata, boundary=secondboundarydata",
        b"multipart/form-data; boundary=myboundarydata; ",
        b"multipart/form-data; boundary=myboundarydata; boundary=secondboundarydata",
        b"multipart/form-data; boundary=\"myboundarydata\"",
        b"multipart/form-data; boundary=   \"myboundarydata\"",
        b"multipart/form-data; boundary=\"myboundarydata\"  ",
    ];

    for input in inputs {
        let (_, (_, _, _, _, b, _, _, _)) = boundary()(input).unwrap();
        assert_eq!(b, b"myboundarydata");
    }

    let (_, (_, _, _, _, b, _, _, _)) =
        boundary()(b"multipart/form-data; boundary=\"myboundarydata").unwrap();
    assert_eq!(b, b"\"myboundarydata");

    let (_, (_, _, _, _, b, _, _, _)) =
        boundary()(b"multipart/form-data; boundary=   myboundarydata\"").unwrap();
    assert_eq!(b, b"myboundarydata\"");
}

#[test]
fn ValidateBoundary() {
    let inputs: Vec<&[u8]> = vec![
        b"Unusual\'Boundary",
        b"Unusual(Boundary",
        b"Unusual)Boundary",
        b"Unusual+Boundary",
        b"Unusual_Boundary",
        b"Unusual,Boundary",
        b"Unusual.Boundary",
        b"Unusual/Boundary",
        b"Unusual:Boundary",
        b"Unusual=Boundary",
        b"Unusual?Boundary",
        b"Invalid>Boundary",
        b"InvalidBoundaryTOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOLONG",
        b"", //Invalid...Need at least one byte
        b"InvalidUnusual.~Boundary",
    ];
    let outputs: Vec<MultipartFlags> = vec![
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID
            | MultipartFlags::HTP_MULTIPART_HBOUNDARY_UNUSUAL,
    ];

    for i in 0..inputs.len() {
        let mut flags = MultipartFlags::empty();
        validate_boundary(inputs[i], &mut flags);
        assert_eq!(outputs[i], flags);
    }
}

#[test]
fn ValidateContentType() {
    let inputs: Vec<&[u8]> = vec![
        b"multipart/form-data; boundary   = stuff, boundary=stuff",
        b"multipart/form-data; boundary=stuffm BounDary=stuff",
        b"multipart/form-data; Boundary=stuff",
        b"multipart/form-data; bouNdary=stuff",
        b"multipart/form-data; boundary=stuff",
    ];
    let outputs: Vec<MultipartFlags> = vec![
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::HTP_MULTIPART_HBOUNDARY_INVALID,
        MultipartFlags::empty(),
    ];

    for i in 0..inputs.len() {
        let mut flags = MultipartFlags::empty();
        validate_content_type(inputs[i], &mut flags);
        assert_eq!(outputs[i], flags);
    }
}

// Tests

#[test]
fn Header() {
    // Space after header name
    let input: &[u8] =
        b"Content-Disposition: form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    let name: &[u8] = b"Content-Disposition";
    let value: &[u8] = b"form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert_eq!((name, value), header()(input).unwrap().1);

    // Tab after header name
    let input: &[u8] =
        b"Content-Disposition:\tform-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    let name: &[u8] = b"Content-Disposition";
    let value: &[u8] = b"form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert_eq!((name, value), header()(input).unwrap().1);

    // Space/tabs after header name
    let input: &[u8] =
        b"Content-Disposition: \t form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    let name: &[u8] = b"Content-Disposition";
    let value: &[u8] = b"form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert_eq!((name, value), header()(input).unwrap().1);

    // No space after header name
    let input: &[u8] =
        b"Content-Disposition:form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    let name: &[u8] = b"Content-Disposition";
    let value: &[u8] = b"form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert_eq!((name, value), header()(input).unwrap().1);

    // Space before header name
    let input: &[u8] =
        b" Content-Disposition: form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert!(header()(input).is_err());

    // Null characters
    let input: &[u8] =
        b"Content-Disposition\0: form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert!(header()(input).is_err());

    // Empty header name
    let input: &[u8] = b": form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert!(header()(input).is_err());

    // Empty header value
    let input: &[u8] = b"Content-Disposition:  ";
    assert!(header()(input).is_err());

    // Invalid header name characters
    let input: &[u8] =
        b"Content-Disposition\r\n:form-data; name=\"file1\"; filename=\"file.bin\"\r\n\"";
    assert!(header()(input).is_err());
}
