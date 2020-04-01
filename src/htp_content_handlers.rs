use ::libc;
extern "C" {
    #[no_mangle]
    fn calloc(_: libc::c_ulong, _: libc::c_ulong) -> *mut libc::c_void;
    #[no_mangle]
    fn free(__ptr: *mut libc::c_void);
    #[no_mangle]
    fn htp_list_array_get(
        l: *const crate::src::htp_list::htp_list_array_t,
        idx: size_t,
    ) -> *mut libc::c_void;
    #[no_mangle]
    fn htp_list_array_size(l: *const crate::src::htp_list::htp_list_array_t) -> size_t;
    #[no_mangle]
    fn bstr_begins_with_c(bhaystack: *const bstr, cneedle: *const libc::c_char) -> libc::c_int;
    #[no_mangle]
    fn bstr_free(b: *mut bstr);
    #[no_mangle]
    fn htp_table_destroy_ex(table: *mut crate::src::htp_table::htp_table_t);
    #[no_mangle]
    fn htp_table_get_c(
        table: *const crate::src::htp_table::htp_table_t,
        ckey: *const libc::c_char,
    ) -> *mut libc::c_void;
    #[no_mangle]
    fn htp_table_get_index(
        table: *const crate::src::htp_table::htp_table_t,
        idx: size_t,
        key: *mut *mut bstr,
    ) -> *mut libc::c_void;
    #[no_mangle]
    fn htp_table_size(table: *const crate::src::htp_table::htp_table_t) -> size_t;
    #[no_mangle]
    fn htp_mpartp_create(
        cfg: *mut crate::src::htp_config::htp_cfg_t,
        boundary: *mut bstr,
        flags: uint64_t,
    ) -> *mut crate::src::htp_multipart::htp_mpartp_t;
    #[no_mangle]
    fn htp_mpartp_find_boundary(
        content_type: *mut bstr,
        boundary: *mut *mut bstr,
        multipart_flags: *mut uint64_t,
    ) -> htp_status_t;
    #[no_mangle]
    fn htp_mpartp_get_multipart(
        parser: *mut crate::src::htp_multipart::htp_mpartp_t,
    ) -> *mut crate::src::htp_multipart::htp_multipart_t;
    #[no_mangle]
    fn htp_mpartp_finalize(parser: *mut crate::src::htp_multipart::htp_mpartp_t) -> htp_status_t;
    #[no_mangle]
    fn htp_mpartp_parse(
        parser: *mut crate::src::htp_multipart::htp_mpartp_t,
        data: *const libc::c_void,
        len: size_t,
    ) -> htp_status_t;
    #[no_mangle]
    fn htp_tx_register_request_body_data(
        tx: *mut crate::src::htp_transaction::htp_tx_t,
        callback_fn: Option<
            unsafe extern "C" fn(_: *mut crate::src::htp_transaction::htp_tx_data_t) -> libc::c_int,
        >,
    );
    #[no_mangle]
    fn htp_tx_req_add_param(
        tx: *mut crate::src::htp_transaction::htp_tx_t,
        param: *mut crate::src::htp_transaction::htp_param_t,
    ) -> htp_status_t;
    #[no_mangle]
    fn htp_urlenp_create(
        tx: *mut crate::src::htp_transaction::htp_tx_t,
    ) -> *mut crate::src::htp_urlencoded::htp_urlenp_t;
    #[no_mangle]
    fn htp_urlenp_destroy(urlenp: *mut crate::src::htp_urlencoded::htp_urlenp_t);
    #[no_mangle]
    fn htp_urlenp_parse_partial(
        urlenp: *mut crate::src::htp_urlencoded::htp_urlenp_t,
        data: *const libc::c_void,
        len: size_t,
    ) -> htp_status_t;
    #[no_mangle]
    fn htp_urlenp_parse_complete(
        urlenp: *mut crate::src::htp_urlencoded::htp_urlenp_t,
        data: *const libc::c_void,
        len: size_t,
    ) -> htp_status_t;
    #[no_mangle]
    fn htp_urlenp_finalize(urlenp: *mut crate::src::htp_urlencoded::htp_urlenp_t) -> htp_status_t;
}
pub type __uint8_t = libc::c_uchar;
pub type __uint16_t = libc::c_ushort;
pub type __int32_t = libc::c_int;
pub type __int64_t = libc::c_long;
pub type __uint64_t = libc::c_ulong;
pub type __time_t = libc::c_long;
pub type __suseconds_t = libc::c_long;
pub type size_t = libc::c_ulong;
pub type int32_t = __int32_t;
pub type int64_t = __int64_t;
pub type uint8_t = __uint8_t;
pub type uint16_t = __uint16_t;
pub type uint64_t = __uint64_t;

pub type htp_status_t = libc::c_int;

pub type bstr = crate::src::bstr::bstr_t;

pub type htp_time_t = libc::timeval;

/* *
 * This callback function feeds request body data to a Urlencoded parser
 * and, later, feeds the parsed parameters to the correct structures.
 *
 * @param[in] d
 * @return HTP_OK on success, HTP_ERROR on failure.
 */
#[no_mangle]
pub unsafe extern "C" fn htp_ch_urlencoded_callback_request_body_data(
    mut d: *mut crate::src::htp_transaction::htp_tx_data_t,
) -> htp_status_t {
    let mut tx: *mut crate::src::htp_transaction::htp_tx_t = (*d).tx;
    // Check that we were not invoked again after the finalization.
    if (*(*tx).request_urlenp_body).params.is_null() {
        return -(1 as libc::c_int);
    }
    if !(*d).data.is_null() {
        // Process one chunk of data.
        htp_urlenp_parse_partial(
            (*tx).request_urlenp_body,
            (*d).data as *const libc::c_void,
            (*d).len,
        );
    } else {
        // Finalize parsing.
        htp_urlenp_finalize((*tx).request_urlenp_body);
        // Add all parameters to the transaction.
        let mut name: *mut bstr = 0 as *mut bstr;
        let mut value: *mut bstr = 0 as *mut bstr;
        let mut i: size_t = 0 as libc::c_int as size_t;
        let mut n: size_t = htp_table_size((*(*tx).request_urlenp_body).params);
        while i < n {
            value =
                htp_table_get_index((*(*tx).request_urlenp_body).params, i, &mut name) as *mut bstr;
            let mut param: *mut crate::src::htp_transaction::htp_param_t = calloc(
                1 as libc::c_int as libc::c_ulong,
                ::std::mem::size_of::<crate::src::htp_transaction::htp_param_t>() as libc::c_ulong,
            )
                as *mut crate::src::htp_transaction::htp_param_t;
            if param.is_null() {
                return -(1 as libc::c_int);
            }
            (*param).name = name;
            (*param).value = value;
            (*param).source = crate::src::htp_transaction::htp_data_source_t::HTP_SOURCE_BODY;
            (*param).parser_id =
                crate::src::htp_transaction::htp_parser_id_t::HTP_PARSER_URLENCODED;
            (*param).parser_data = 0 as *mut libc::c_void;
            if htp_tx_req_add_param(tx, param) != 1 as libc::c_int {
                free(param as *mut libc::c_void);
                return -(1 as libc::c_int);
            }
            i = i.wrapping_add(1)
        }
        // All the parameter data is now owned by the transaction, and
        // the parser table used to store it is no longer needed. The
        // line below will destroy just the table, leaving keys intact.
        htp_table_destroy_ex((*(*tx).request_urlenp_body).params);
        (*(*tx).request_urlenp_body).params = 0 as *mut crate::src::htp_table::htp_table_t
    }
    return 1 as libc::c_int;
}

/* *
 * Determine if the request has a Urlencoded body, and, if it does, create and
 * attach an instance of the Urlencoded parser to the transaction.
 *
 * @param[in] connp
 * @return HTP_OK if a new parser has been setup, HTP_DECLINED if the MIME type
 *         is not appropriate for this parser, and HTP_ERROR on failure.
 */
#[no_mangle]
pub unsafe extern "C" fn htp_ch_urlencoded_callback_request_headers(
    mut tx: *mut crate::src::htp_transaction::htp_tx_t,
) -> htp_status_t {
    // Check the request content type to see if it matches our MIME type.
    if (*tx).request_content_type.is_null()
        || bstr_begins_with_c(
            (*tx).request_content_type,
            b"application/x-www-form-urlencoded\x00" as *const u8 as *const libc::c_char,
        ) == 0
    {
        return 0 as libc::c_int;
    }
    // Create parser instance.
    (*tx).request_urlenp_body = htp_urlenp_create(tx);
    if (*tx).request_urlenp_body.is_null() {
        return -(1 as libc::c_int);
    }
    // Register a request body data callback.
    htp_tx_register_request_body_data(
        tx,
        Some(
            htp_ch_urlencoded_callback_request_body_data
                as unsafe extern "C" fn(
                    _: *mut crate::src::htp_transaction::htp_tx_data_t,
                ) -> htp_status_t,
        ),
    );
    return 1 as libc::c_int;
}

/* *
 * Parses request query string, if present.
 *
 * @param[in] connp
 * @param[in] raw_data
 * @param[in] raw_len
 * @return HTP_OK if query string was parsed, HTP_DECLINED if there was no query
 *         string, and HTP_ERROR on failure.
 */
#[no_mangle]
pub unsafe extern "C" fn htp_ch_urlencoded_callback_request_line(
    mut tx: *mut crate::src::htp_transaction::htp_tx_t,
) -> htp_status_t {
    // Proceed only if there's something for us to parse.
    if (*(*tx).parsed_uri).query.is_null()
        || (*(*(*tx).parsed_uri).query).len == 0 as libc::c_int as libc::c_ulong
    {
        return 0 as libc::c_int;
    }
    // We have a non-zero length query string.
    (*tx).request_urlenp_query = htp_urlenp_create(tx);
    if (*tx).request_urlenp_query.is_null() {
        return -(1 as libc::c_int);
    }
    if htp_urlenp_parse_complete(
        (*tx).request_urlenp_query,
        (if (*(*(*tx).parsed_uri).query).realptr.is_null() {
            ((*(*tx).parsed_uri).query as *mut libc::c_uchar)
                .offset(::std::mem::size_of::<bstr>() as libc::c_ulong as isize)
        } else {
            (*(*(*tx).parsed_uri).query).realptr
        }) as *const libc::c_void,
        (*(*(*tx).parsed_uri).query).len,
    ) != 1 as libc::c_int
    {
        htp_urlenp_destroy((*tx).request_urlenp_query);
        return -(1 as libc::c_int);
    }
    // Add all parameters to the transaction.
    let mut name: *mut bstr = 0 as *mut bstr;
    let mut value: *mut bstr = 0 as *mut bstr;
    let mut i: size_t = 0 as libc::c_int as size_t;
    let mut n: size_t = htp_table_size((*(*tx).request_urlenp_query).params);
    while i < n {
        value =
            htp_table_get_index((*(*tx).request_urlenp_query).params, i, &mut name) as *mut bstr;
        let mut param: *mut crate::src::htp_transaction::htp_param_t = calloc(
            1 as libc::c_int as libc::c_ulong,
            ::std::mem::size_of::<crate::src::htp_transaction::htp_param_t>() as libc::c_ulong,
        )
            as *mut crate::src::htp_transaction::htp_param_t;
        if param.is_null() {
            return -(1 as libc::c_int);
        }
        (*param).name = name;
        (*param).value = value;
        (*param).source = crate::src::htp_transaction::htp_data_source_t::HTP_SOURCE_QUERY_STRING;
        (*param).parser_id = crate::src::htp_transaction::htp_parser_id_t::HTP_PARSER_URLENCODED;
        (*param).parser_data = 0 as *mut libc::c_void;
        if htp_tx_req_add_param(tx, param) != 1 as libc::c_int {
            free(param as *mut libc::c_void);
            return -(1 as libc::c_int);
        }
        i = i.wrapping_add(1)
    }
    // All the parameter data is now owned by the transaction, and
    // the parser table used to store it is no longer needed. The
    // line below will destroy just the table, leaving keys intact.
    htp_table_destroy_ex((*(*tx).request_urlenp_query).params);
    (*(*tx).request_urlenp_query).params = 0 as *mut crate::src::htp_table::htp_table_t;
    htp_urlenp_destroy((*tx).request_urlenp_query);
    (*tx).request_urlenp_query = 0 as *mut crate::src::htp_urlencoded::htp_urlenp_t;
    return 1 as libc::c_int;
}

/* *
 * Finalize Multipart processing.
 *
 * @param[in] d
 * @return HTP_OK on success, HTP_ERROR on failure.
 */
#[no_mangle]
pub unsafe extern "C" fn htp_ch_multipart_callback_request_body_data(
    mut d: *mut crate::src::htp_transaction::htp_tx_data_t,
) -> htp_status_t {
    let mut tx: *mut crate::src::htp_transaction::htp_tx_t = (*d).tx;
    // Check that we were not invoked again after the finalization.
    if (*(*tx).request_mpartp).gave_up_data == 1 as libc::c_int {
        return -(1 as libc::c_int);
    }
    if !(*d).data.is_null() {
        // Process one chunk of data.
        htp_mpartp_parse(
            (*tx).request_mpartp,
            (*d).data as *const libc::c_void,
            (*d).len,
        );
    } else {
        // Finalize parsing.
        htp_mpartp_finalize((*tx).request_mpartp);
        let mut body: *mut crate::src::htp_multipart::htp_multipart_t =
            htp_mpartp_get_multipart((*tx).request_mpartp);
        let mut i: size_t = 0 as libc::c_int as size_t;
        let mut n: size_t = htp_list_array_size((*body).parts);
        while i < n {
            let mut part: *mut crate::src::htp_multipart::htp_multipart_part_t =
                htp_list_array_get((*body).parts, i)
                    as *mut crate::src::htp_multipart::htp_multipart_part_t;
            // Use text parameters.
            if (*part).type_0
                == crate::src::htp_multipart::htp_multipart_type_t::MULTIPART_PART_TEXT
            {
                let mut param: *mut crate::src::htp_transaction::htp_param_t = calloc(
                    1 as libc::c_int as libc::c_ulong,
                    ::std::mem::size_of::<crate::src::htp_transaction::htp_param_t>()
                        as libc::c_ulong,
                )
                    as *mut crate::src::htp_transaction::htp_param_t;
                if param.is_null() {
                    return -(1 as libc::c_int);
                }
                (*param).name = (*part).name;
                (*param).value = (*part).value;
                (*param).source = crate::src::htp_transaction::htp_data_source_t::HTP_SOURCE_BODY;
                (*param).parser_id =
                    crate::src::htp_transaction::htp_parser_id_t::HTP_PARSER_MULTIPART;
                (*param).parser_data = part as *mut libc::c_void;
                if htp_tx_req_add_param(tx, param) != 1 as libc::c_int {
                    free(param as *mut libc::c_void);
                    return -(1 as libc::c_int);
                }
            }
            i = i.wrapping_add(1)
        }
        // Tell the parser that it no longer owns names
        // and values of MULTIPART_PART_TEXT parts.
        (*(*tx).request_mpartp).gave_up_data = 1 as libc::c_int
    }
    return 1 as libc::c_int;
}

/* *
 * Inspect request headers and register the Multipart request data hook
 * if it contains a multipart/form-data body.
 *
 * @param[in] connp
 * @return HTP_OK if a new parser has been setup, HTP_DECLINED if the MIME type
 *         is not appropriate for this parser, and HTP_ERROR on failure.
 */
#[no_mangle]
pub unsafe extern "C" fn htp_ch_multipart_callback_request_headers(
    mut tx: *mut crate::src::htp_transaction::htp_tx_t,
) -> htp_status_t {
    // The field tx->request_content_type does not contain the entire C-T
    // value and so we cannot use it to look for a boundary, but we can
    // use it for a quick check to determine if the C-T header exists.
    if (*tx).request_content_type.is_null() {
        return 0 as libc::c_int;
    }
    // Look for a boundary.
    let mut ct: *mut crate::src::htp_transaction::htp_header_t = htp_table_get_c(
        (*tx).request_headers,
        b"content-type\x00" as *const u8 as *const libc::c_char,
    )
        as *mut crate::src::htp_transaction::htp_header_t;
    if ct.is_null() {
        return -(1 as libc::c_int);
    }
    let mut boundary: *mut bstr = 0 as *mut bstr;
    let mut flags: uint64_t = 0 as libc::c_int as uint64_t;
    let mut rc: htp_status_t = htp_mpartp_find_boundary((*ct).value, &mut boundary, &mut flags);
    if rc != 1 as libc::c_int {
        // No boundary (HTP_DECLINED) or error (HTP_ERROR).
        return rc;
    }
    if boundary.is_null() {
        return -(1 as libc::c_int);
    }
    // Create a Multipart parser instance.
    (*tx).request_mpartp = htp_mpartp_create((*(*tx).connp).cfg, boundary, flags);
    if (*tx).request_mpartp.is_null() {
        bstr_free(boundary);
        return -(1 as libc::c_int);
    }
    // Configure file extraction.
    if (*(*tx).cfg).extract_request_files != 0 {
        (*(*tx).request_mpartp).extract_files = 1 as libc::c_int;
        (*(*tx).request_mpartp).extract_dir = (*(*(*tx).connp).cfg).tmpdir
    }
    // Register a request body data callback.
    htp_tx_register_request_body_data(
        tx,
        Some(
            htp_ch_multipart_callback_request_body_data
                as unsafe extern "C" fn(
                    _: *mut crate::src::htp_transaction::htp_tx_data_t,
                ) -> htp_status_t,
        ),
    );
    return 1 as libc::c_int;
}
