use crate::error::Result;
use crate::htp_transaction::Protocol;
use crate::htp_util::Flags;
use crate::htp_util::*;
use crate::{
    bstr, htp_config, htp_connection_parser, htp_parsers, htp_request, htp_transaction, htp_util,
    Status,
};
use nom::bytes::complete::take_while;
use nom::error::ErrorKind;
use nom::sequence::tuple;
use std::cmp::Ordering;

impl htp_connection_parser::htp_connp_t {
    /// Extract one request header. A header can span multiple lines, in
    /// which case they will be folded into one before parsing is attempted.
    ///
    /// Returns HTP_OK or HTP_ERROR
    pub unsafe fn process_request_header_generic(&mut self, data: &[u8]) -> Result<()> {
        // Try to parse the header.
        let header = self.parse_request_header_generic(data)?;
        let mut repeated = false;
        let reps = self.in_tx_mut_ok()?.req_header_repetitions;
        let mut update_reps = false;
        // Do we already have a header with the same name?
        if let Some((_, h_existing)) = self
            .in_tx_mut_ok()?
            .request_headers
            .get_nocase_mut(header.name.as_slice())
        {
            // TODO Do we want to have a list of the headers that are
            //      allowed to be combined in this way?
            if !h_existing.flags.contains(Flags::HTP_FIELD_REPEATED) {
                // This is the second occurence for this header.
                repeated = true;
            } else if reps < 64 {
                update_reps = true;
            } else {
                return Ok(());
            }
            // For simplicity reasons, we count the repetitions of all headers
            // Keep track of repeated same-name headers.
            h_existing.flags |= Flags::HTP_FIELD_REPEATED;
            // Having multiple C-L headers is against the RFC but
            // servers may ignore the subsequent headers if the values are the same.
            if header.name.cmp_nocase("Content-Length") == Ordering::Equal {
                // Don't use string comparison here because we want to
                // ignore small formatting differences.
                let existing_cl = htp_util::htp_parse_content_length(&h_existing.value, None);
                let new_cl = htp_util::htp_parse_content_length(&header.value, None);
                // Ambiguous response C-L value.
                if existing_cl.is_none() || new_cl.is_none() || existing_cl != new_cl {
                    htp_warn!(
                        self as *mut htp_connection_parser::htp_connp_t,
                        htp_log_code::DUPLICATE_CONTENT_LENGTH_FIELD_IN_REQUEST,
                        "Ambiguous request C-L value"
                    );
                }
            } else {
                // Add to the existing header.
                h_existing.value.extend_from_slice(b", ");
                h_existing.value.extend_from_slice(header.value.as_slice());
            }
        } else {
            self.in_tx_mut_ok()?
                .request_headers
                .add(header.name.clone(), header);
        }
        if update_reps {
            self.in_tx_mut_ok()?.req_header_repetitions =
                self.in_tx_mut_ok()?.req_header_repetitions.wrapping_add(1)
        }
        if repeated {
            htp_warn!(
                self as *mut htp_connection_parser::htp_connp_t,
                htp_log_code::REQUEST_HEADER_REPETITION,
                "Repetition for header"
            );
        }
        Ok(())
    }

    /// Generic request header parser.
    pub unsafe fn parse_request_header_generic(
        &mut self,
        data: &[u8],
    ) -> Result<htp_transaction::htp_header_t> {
        let mut flags = Flags::empty();
        let data = htp_util::htp_chomp(&data);

        let (name, value): (&[u8], &[u8]) = match htp_util::split_by_colon(data) {
            Ok((mut name, mut value)) => {
                // Empty header name.
                if name.is_empty() {
                    flags |= Flags::HTP_FIELD_INVALID;
                    // Log only once per transaction.
                    if !self
                        .in_tx_mut_ok()?
                        .flags
                        .contains(Flags::HTP_FIELD_INVALID)
                    {
                        flags |= Flags::HTP_FIELD_INVALID;
                        htp_warn!(
                            self as *mut htp_connection_parser::htp_connp_t,
                            htp_log_code::REQUEST_INVALID_EMPTY_NAME,
                            "Request field invalid: empty name"
                        );
                    }
                }
                // Ignore LWS after field-name.
                if let Ok((name_remaining, tws)) = take_is_space_trailing(name) {
                    flags |= Flags::HTP_FIELD_INVALID;
                    if !tws.is_empty() {
                        // Log only once per transaction.
                        if !self
                            .in_tx_mut_ok()?
                            .flags
                            .contains(Flags::HTP_FIELD_INVALID)
                        {
                            flags |= Flags::HTP_FIELD_INVALID;
                            htp_warn!(
                                self as *mut htp_connection_parser::htp_connp_t,
                                htp_log_code::REQUEST_INVALID_LWS_AFTER_NAME,
                                "Request field invalid: LWS after name"
                            );
                        }
                    }
                    name = name_remaining;
                }
                // Remove value characters after null
                if let Ok((_, val_before_null)) = htp_util::take_until_null(value) {
                    value = val_before_null;
                }
                // Remove value trailing whitespace
                if let Ok((val_remaining, _)) = take_is_space_trailing(value) {
                    value = val_remaining;
                }

                // Check that field-name is a token
                if !htp_util::is_word_token(name) {
                    // Incorrectly formed header name.
                    flags |= Flags::HTP_FIELD_INVALID;
                    // Log only once per transaction.
                    if !self
                        .in_tx_mut_ok()?
                        .flags
                        .contains(Flags::HTP_FIELD_INVALID)
                    {
                        self.in_tx_mut_ok()?.flags |= Flags::HTP_FIELD_INVALID;
                        htp_warn!(
                            self as *mut htp_connection_parser::htp_connp_t,
                            htp_log_code::REQUEST_HEADER_INVALID,
                            "Request header name is not a token"
                        );
                    }
                }
                (name, value)
            }
            _ => {
                // No colon
                flags |= Flags::HTP_FIELD_UNPARSEABLE;
                // Log only once per transaction.
                if !self
                    .in_tx_mut_ok()?
                    .flags
                    .contains(Flags::HTP_FIELD_UNPARSEABLE)
                {
                    self.in_tx_mut_ok()?.flags |= Flags::HTP_FIELD_UNPARSEABLE;
                    htp_warn!(
                        self as *mut htp_connection_parser::htp_connp_t,
                        htp_log_code::REQUEST_FIELD_MISSING_COLON,
                        "Request field invalid: colon missing"
                    );
                }
                // We handle this case as a header with an empty name, with the value equal
                // to the entire input string.
                // TODO Apache will respond to this problem with a 400.
                // Now extract the name and the value
                (b"", data)
            }
        };

        Ok(htp_transaction::htp_header_t::new_with_flags(
            name.into(),
            value.into(),
            flags,
        ))
    }

    /// Generic request line parser.
    ///
    /// Returns HTP_OK or HTP_ERROR
    pub unsafe fn parse_request_line_generic(&mut self) -> Result<()> {
        self.parse_request_line_generic_ex(0)
    }

    pub unsafe fn parse_request_line_generic_ex(&mut self, nul_terminates: i32) -> Result<()> {
        let mut mstart: bool = false;
        let data = if let Some(data) = self.in_tx_mut_ok()?.request_line.clone() {
            data
        } else {
            return Err(Status::ERROR);
        };
        let mut data = data.as_slice();
        if nul_terminates != 0 {
            if let Ok((_, before_null)) = htp_util::take_until_null(data) {
                data = before_null
            }
        }

        // The request method starts at the beginning of the
        // line and ends with the first whitespace character.
        let method_parser = tuple::<_, _, (_, ErrorKind), _>
                                // skip past leading whitespace. IIS allows this
                               ((take_htp_is_space,
                               take_not_htp_is_space,
                                // Ignore whitespace after request method. The RFC allows
                                 // for only one SP, but then suggests any number of SP and HT
                                 // should be permitted. Apache uses isspace(), which is even
                                 // more permitting, so that's what we use here.
                               htp_util::take_ascii_whitespace()
                               ));

        if let Ok((remaining, (ls, method, ws))) = method_parser(data) {
            if !ls.is_empty() {
                htp_warn!(
                    self as *mut htp_connection_parser::htp_connp_t,
                    htp_log_code::REQUEST_LINE_LEADING_WHITESPACE,
                    "Request line: leading whitespace"
                );

                if (*(*self).cfg).requestline_leading_whitespace_unwanted
                    != htp_config::htp_unwanted_t::HTP_UNWANTED_IGNORE
                {
                    // reset mstart so that we copy the whitespace into the method
                    mstart = true;
                    // set expected response code to this anomaly
                    self.in_tx_mut_ok()?.response_status_expected_number =
                        (*(*self).cfg).requestline_leading_whitespace_unwanted
                }
            }

            if mstart {
                self.in_tx_mut_ok()?.request_method =
                    Some(bstr::bstr_t::from([&ls[..], &method[..]].concat()));
            } else {
                self.in_tx_mut_ok()?.request_method = Some(bstr::bstr_t::from(method));
            }

            if let Some(request_method) = &self.in_tx_mut_ok()?.request_method {
                self.in_tx_mut_ok()?.request_method_number =
                    htp_util::htp_convert_bstr_to_method(&request_method);
            }

            // Too much performance overhead for fuzzing
            if ws.iter().any(|&c| c != 0x20) {
                htp_warn!(
                    self as *mut htp_connection_parser::htp_connp_t,
                    htp_log_code::METHOD_DELIM_NON_COMPLIANT,
                    "Request line: non-compliant delimiter between Method and URI"
                );
            }

            if remaining.is_empty() {
                // No, this looks like a HTTP/0.9 request.
                self.in_tx_mut_ok()?.is_protocol_0_9 = 1;
                self.in_tx_mut_ok()?.request_protocol_number = Protocol::V0_9;
                if self.in_tx_mut_ok()?.request_method_number
                    == htp_request::htp_method_t::HTP_M_UNKNOWN
                {
                    htp_warn!(
                        self as *mut htp_connection_parser::htp_connp_t,
                        htp_log_code::REQUEST_LINE_UNKNOWN_METHOD,
                        "Request line: unknown method only"
                    );
                }
                return Ok(());
            }

            let uri_protocol_parser = tuple::<_, _, (_, ErrorKind), _>
            // The URI ends with the first whitespace.
            ((take_while(|c: u8| c != 0x20),
              // Ignore whitespace after URI.
              take_htp_is_space)
            );

            if let Ok((mut protocol, (mut uri, _))) = uri_protocol_parser(remaining) {
                if uri.len() == remaining.len() && uri.iter().any(|&c| htp_is_space(c)) {
                    // warn regardless if we've seen non-compliant chars
                    htp_warn!(
                        self as *mut htp_connection_parser::htp_connp_t,
                        htp_log_code::URI_DELIM_NON_COMPLIANT,
                        "Request line: URI contains non-compliant delimiter"
                    );
                    // if we've seen some 'bad' delimiters, we retry with those
                    let uri_protocol_parser2 = tuple::<_, _, (_, ErrorKind), _>((
                        take_not_htp_is_space,
                        take_htp_is_space,
                    ));
                    if let Ok((protocol2, (uri2, _))) = uri_protocol_parser2(remaining) {
                        uri = uri2;
                        protocol = protocol2;
                    }
                }
                self.in_tx_mut_ok()?.request_uri = Some(bstr::bstr_t::from(uri));
                // Is there protocol information available?
                if protocol.is_empty() {
                    // No, this looks like a HTTP/0.9 request.
                    self.in_tx_mut_ok()?.is_protocol_0_9 = 1;
                    self.in_tx_mut_ok()?.request_protocol_number = Protocol::V0_9;
                    if self.in_tx_mut_ok()?.request_method_number
                        == htp_request::htp_method_t::HTP_M_UNKNOWN
                    {
                        htp_warn!(
                            self as *mut htp_connection_parser::htp_connp_t,
                            htp_log_code::REQUEST_LINE_UNKNOWN_METHOD_NO_PROTOCOL,
                            "Request line: unknown method and no protocol"
                        );
                    }
                    return Ok(());
                }
                // The protocol information continues until the end of the line.
                self.in_tx_mut_ok()?.request_protocol = Some(bstr::bstr_t::from(protocol));
                self.in_tx_mut_ok()?.request_protocol_number =
                    htp_parsers::htp_parse_protocol(protocol, &mut *self);
                if self.in_tx_mut_ok()?.request_method_number
                    == htp_request::htp_method_t::HTP_M_UNKNOWN
                    && self.in_tx_mut_ok()?.request_protocol_number == Protocol::INVALID
                {
                    htp_warn!(
                        self as *mut htp_connection_parser::htp_connp_t,
                        htp_log_code::REQUEST_LINE_UNKNOWN_METHOD_INVALID_PROTOCOL,
                        "Request line: unknown method and invalid protocol"
                    );
                }
            }
        }
        Ok(())
    }
}
