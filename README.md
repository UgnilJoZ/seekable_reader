[![Crates.io](https://img.shields.io/crates/v/seekable_reader.svg)](https://crates.io/crates/seekable_reader)
[![Documentation](https://docs.rs/seekable_reader/badge.svg)](https://docs.rs/crate/seekable_reader/)
[![Dependency Status](https://deps.rs/crate/seekable_reader/0.1.2/status.svg)](https://deps.rs/crate/seekable_reader/0.1.2)

# seekable_reader
This crate introduces the **SeekableReader**, which provides `Seek` if wrapped around a `Read` instance.

An example:
```rust
use std::io::{Read, Seek, SeekFrom};
use seekable_reader::SeekableReader;

let source = vec![1, 2, 3, 4, 5];
let mut reader = SeekableReader::new(source.as_slice(), 1);
let mut buffer = vec![0; 5];
// Read one byte and seek back
reader.read(&mut buffer[..1]).unwrap();
reader.seek(SeekFrom::Start(0)).unwrap();
// First byte can be read again!
let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
assert_eq!(&source, &bytes);
```