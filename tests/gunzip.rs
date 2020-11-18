#![allow(non_snake_case)]
use htp::{
    bstr::*,
    c_api::{htp_connp_create, htp_connp_destroy_all},
    config::{create, Config, HtpServerPersonality},
    connection_parser::ConnectionParser,
    decompressors::{Decompressor, HtpContentEncoding},
    transaction::{Data, Transaction},
    HtpStatus,
};
use std::{env, path::PathBuf};

// import common testing utilities
mod common;
use common::htp_connp_tx_create;

#[no_mangle]
extern "C" fn GUnzip_decompressor_callback(d: *mut Data) -> HtpStatus {
    unsafe {
        let output_ptr: *mut Bstr = (*(*d).tx()).user_data() as *mut Bstr;
        (*output_ptr).add((*d).as_slice().unwrap());
    }
    HtpStatus::OK
}

#[derive(Debug)]
struct Test {
    cfg: *mut Config,
    connp: *mut ConnectionParser,
    output: Bstr,
    expected: Bstr,
    tx: *mut Transaction,
    decompressor: Decompressor,
}

enum TestError {
    Io(std::io::Error),
    Htp(HtpStatus),
}

impl Test {
    fn new() -> Self {
        unsafe {
            let cfg = create();
            assert!(!cfg.is_null());
            (*cfg)
                .set_server_personality(HtpServerPersonality::APACHE_2)
                .unwrap();
            // The default bomb limit may be slow in some development environments causing tests to fail.
            (*cfg).compression_options.set_time_limit(std::u32::MAX);
            let connp = htp_connp_create(cfg);
            assert!(!connp.is_null());
            let tx = htp_connp_tx_create(connp);
            assert!(!tx.is_null());

            let output = Bstr::new();
            let expected = Bstr::from("The five boxing wizards jump quickly.");

            Test {
                cfg,
                connp,
                output,
                expected,
                tx,
                decompressor: Decompressor::new_with_callback(
                    HtpContentEncoding::GZIP,
                    Box::new(move |data: Option<&[u8]>| {
                        let mut tx_data = Data::new(tx, data, false);
                        GUnzip_decompressor_callback(&mut tx_data);
                        Ok(tx_data.len())
                    }),
                    Default::default(),
                )
                .unwrap(),
            }
        }
    }

    fn run(&mut self, filename: &str) -> Result<(), TestError> {
        let mut filepath = if let Ok(dir) = std::env::var("srcdir") {
            PathBuf::from(dir)
        } else {
            let mut base = PathBuf::from(
                env::var("CARGO_MANIFEST_DIR").expect("Could not determine test file directory"),
            );
            base.push("tests");
            base.push("files");
            base
        };
        filepath.push(filename);

        let data = std::fs::read(filepath).map_err(TestError::Io)?;
        unsafe {
            let output_ptr: *mut Bstr = &mut self.output;
            (*self.tx).set_user_data(output_ptr as *mut core::ffi::c_void);

            self.decompressor
                .decompress(&data)
                .map(|_| ())
                .map_err(|_| TestError::Htp(HtpStatus::ERROR))
        }
    }
}

impl Drop for Test {
    fn drop(&mut self) {
        unsafe {
            htp_connp_destroy_all(self.connp);
            (*self.cfg).destroy();
        }
    }
}

#[test]
fn GUnzip_Minimal() {
    let mut t = Test::new();
    assert!(t.run("gztest-01-minimal.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FNAME() {
    let mut t = Test::new();
    assert!(t.run("gztest-02-fname.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FEXTRA() {
    let mut t = Test::new();
    assert!(t.run("gztest-05-fextra.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FTEXT() {
    let mut t = Test::new();
    assert!(t.run("gztest-06-ftext.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_Multipart() {
    let mut t = Test::new();
    assert!(t.run("gztest-10-multipart.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_InvalidExtraFlags() {
    let mut t = Test::new();
    assert!(t.run("gztest-14-invalid-xfl.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_InvalidHeaderCrc() {
    let mut t = Test::new();
    assert!(t.run("gztest-15-invalid-fhcrc.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

/*
// These tests were disabled in libhtp
#[test]
fn GUnzip_FCOMMENT() {
    let mut t = Test::new();
    assert!(t.run("gztest-03-fcomment.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FHCRC() {
    let mut t = Test::new();
    assert!(t.run("gztest-04-fhcrc.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FRESERVED1() {
    let mut t = Test::new();
    assert!(t.run("gztest-07-freserved1.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FRESERVED2() {
    let mut t = Test::new();
    assert!(t.run("gztest-08-freserved2.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_FRESERVED3() {
    let mut t = Test::new();
    assert!(t.run("gztest-09-freserved3.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_InvalidMethod() {
    let mut t = Test::new();
    assert!(t.run("gztest-11-invalid-method.gz.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_InvalidCrc() {
    let mut t = Test::new();
    assert!(t.run("gztest-12-invalid-crc32.gz").is_ok());
    assert_eq!(t.output, t.expected);
}

#[test]
fn GUnzip_InvalidInputSize() {
    let mut t = Test::new();
    assert!(t.run("gztest-13-invalid-isize.gz").is_ok());
    assert_eq!(t.output, t.expected);
}
*/
