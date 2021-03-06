$(eval CRATE_VERSION=$(shell (test -f Cargo.lock || cargo generate-lockfile) && cargo pkgid | cut -d: -f 3))
$(eval CRATE_VERSION_MAJOR=$(shell echo ${CRATE_VERSION} | cut -d. -f 1))

.DEFAULT_GOAL := all
.PHONY: all
all: htp/.libs/libhtp.so

.PHONY: build
build:
	cargo build --features cbindgen

target/debug/libhtp.so: build

htp/.libs/libhtp.so: target/debug/libhtp.so
	mkdir -p htp/.libs
	cp ./target/debug/libhtp.so htp/.libs/libhtp.so.${CRATE_VERSION}
	ln -sf libhtp.so.${CRATE_VERSION} htp/.libs/libhtp.so.${CRATE_VERSION_MAJOR}
	ln -sf libhtp.so.${CRATE_VERSION} htp/.libs/libhtp.so

# prevents make check from failing in suricata
.PHONY: check
check:

.PHONY: clean
clean:
	rm -f htp/.libs/libhtp.so* htp/htp.h htp/version.h
	rm -f Cargo.lock
	cargo clean -p htp

.PHONY: rpm
rpm: tar
	rpmbuild -bb --define "version ${CRATE_VERSION}" --define "_topdir ${PWD}/target/centos" .rpm/htp.spec

.PHONY: tar
tar: all
	mkdir -p target/_temp/lib
	mkdir -p target/_temp/include/htp
	mkdir -p target/centos/
	mkdir -p target/centos/SOURCES
	cp htp/*.h target/_temp/include/htp
	cp -d htp/.libs/*.so* target/_temp/lib
	tar -czvf target/centos/SOURCES/libhtp-${CRATE_VERSION}.tar.gz target/_temp/ --transform 'flags=r;s#^target/_temp/#libhtp-${CRATE_VERSION}/usr/local/#'
