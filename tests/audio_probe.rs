/// Real world example of seek and read operations obtained through an observer and rodio-rs

use std::io::{Read, Result, Seek, SeekFrom};
use seekable_reader::SeekableReader;

/// Read implementation
struct ExampleRead {
    counter: usize,
}

impl Read for ExampleRead {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        for byte in buf.iter_mut() {
            *byte = self.counter as u8;
            self.counter += 1;
        }
        Ok(buf.len())
    }
}

#[test]
fn complex_read_seek() {
    let reader = ExampleRead {counter: 0};
    let mut reader = SeekableReader::new(reader, 1_048_576);
    let mut buf = vec![0; 2048];
    reader.seek(SeekFrom::Current(0)).unwrap();
    reader.read(&mut buf[..4]).unwrap();
    assert_eq!(buf[0..4], vec![0, 1, 2, 3]);
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.seek(SeekFrom::Current(0)).unwrap();
    reader.read(&mut buf[..2048]).unwrap();
    for i in 0..2048 {
        assert_eq!(buf[i], (i % 256) as u8);
    }
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.seek(SeekFrom::Current(0)).unwrap();
    reader.read(&mut buf[..27]).unwrap();
    for i in 0..27 {
        assert_eq!(buf[i], (i % 256) as u8);
    }
    reader.read(&mut buf[..1024]).unwrap();
    for i in 0..1024 {
        assert_eq!(buf[i], ((i + 27) % 256) as u8);
    }
}