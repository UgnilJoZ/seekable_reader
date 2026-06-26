///
/// This crate introduces the **SeekableReader**, which provides `Seek` if wrapped around a `Read` instance.
///
/// An example:
///  ```
/// use std::io::{Read, Seek, SeekFrom};
/// use seekable_reader::SeekableReader;
///
/// let source = vec![1, 2, 3, 4, 5];
/// let mut reader = SeekableReader::new(source.as_slice(), 1);
/// let mut buffer = vec![0; 5];
/// // Read one byte and seek back
/// reader.read(&mut buffer[..1]).unwrap();
/// reader.seek(SeekFrom::Start(0)).unwrap();
/// // First byte can be read again!
/// let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
/// assert_eq!(&source, &bytes);
/// ```
use core::cmp::min;
use std::io::{Error, ErrorKind, Read, Result, Seek, SeekFrom};
use std::mem;

#[derive(Debug)]
pub enum SeekError {
    /// Tried to seek backwards past the oldest buffered position.
    RewindTooFar {
        requested: u64,
        earliest_possible: u64,
    },
    /// `SeekFrom::End` requires a known stream length and is not supported.
    SeekFromEndUnsupported,
}

impl std::fmt::Display for SeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeekError::RewindTooFar {
                requested,
                earliest_possible,
            } => write!(
                f,
                "cannot seek to position {requested}: earliest buffered position is {earliest_possible}"
            ),
            SeekError::SeekFromEndUnsupported => write!(
                f,
                "SeekFrom::End is not supported for streams with unknown length"
            ),
        }
    }
}

impl std::error::Error for SeekError {}

#[derive(Debug)]
enum Position {
    FrontBuffer(usize),
    BackBuffer(usize),
}

/// A reader adapter that allows to seek a little bit
///
/// The SeekableReader will wrap around a Read instance and can be read normally.
/// The core feature is to provide `Seek`, even if the underlying Reader does not.
/// It achieves this by holding a cache of the read data, which can be read again.
pub struct SeekableReader<R: Read> {
    pub inner: R,
    pub keep_size: usize,
    current_buffer: Vec<u8>,
    older_buffer: Vec<u8>,
    pos: Position,
    /// Bytes read from `inner`
    pub read_bytes: usize,
    buffer_begins_at_pos: usize,
}

impl<R: Read> SeekableReader<R> {
    /// Create a new instance of a SeekableReader.
    ///
    /// It wraps around `inner` and allows seeking backwards by
    /// keeping at least `keep_size` bytes of already read data,
    /// if this amount of data is already read.
    ///
    /// At most, `2 * keep_size` bytes are kept.
    pub fn new(inner: R, keep_size: usize) -> SeekableReader<R> {
        SeekableReader {
            inner,
            keep_size,
            current_buffer: Vec::with_capacity(keep_size),
            older_buffer: Vec::with_capacity(keep_size),
            pos: Position::FrontBuffer(0),
            read_bytes: 0,
            buffer_begins_at_pos: 0,
        }
    }

    // Returns the number of bytes which can be read from inner before the next buffer swap.
    fn remaining_current_buffer_capacity(&self) -> usize {
        self.keep_size - self.current_buffer.len()
    }

    /// Returns the size of the buffered data.
    /// Attempts to seek further back will result an Error.
    pub fn buffered_size(&self) -> usize {
        self.current_buffer.len() + self.older_buffer.len()
    }

    /// Reads more data from `inner` into `buf` and puts them into the cache
    ///
    /// After this operation, the stream position will be at the end of all read data.
    ///
    /// If buf is long enough, the caches will be flushed.
    fn read_inner(&mut self, buf: &mut [u8]) -> Result<usize> {
        let read_bytes = self.inner.read(buf)?;
        let buf = &mut buf[..read_bytes];
        let cache_capacity = 2 * self.keep_size;
        if read_bytes >= cache_capacity - self.current_buffer.len() {
            // Flush cache: keep only the tail of what was just read.
            let skip = cache_capacity * (read_bytes % cache_capacity);
            let (to_older, to_current) =
                (&buf[skip..]).split_at(min(self.keep_size, buf.len() - skip));
            let old_total = self.older_buffer.len() + self.current_buffer.len();
            self.older_buffer.resize(self.keep_size, 0);
            self.older_buffer.as_mut_slice().copy_from_slice(to_older);
            self.current_buffer.resize(to_current.len(), 0);
            self.current_buffer.copy_from_slice(to_current);
            self.buffer_begins_at_pos += old_total + skip;
        } else if read_bytes > self.remaining_current_buffer_capacity() {
            let to_older_size = self.remaining_current_buffer_capacity();
            let old_older_len = self.older_buffer.len();
            mem::swap(&mut self.older_buffer, &mut self.current_buffer);
            let (to_older, to_current) = buf.split_at(min(to_older_size, buf.len()));
            self.older_buffer.extend_from_slice(to_older);
            self.current_buffer.resize(to_current.len(), 0);
            self.current_buffer.copy_from_slice(to_current);
            self.buffer_begins_at_pos += old_older_len;
        } else {
            self.current_buffer.extend_from_slice(buf);
        }
        if self.current_buffer.len() == self.keep_size {
            let old_older_len = self.older_buffer.len();
            mem::swap(&mut self.older_buffer, &mut self.current_buffer);
            self.current_buffer.clear();
            self.buffer_begins_at_pos += old_older_len;
        }
        self.pos = Position::FrontBuffer(self.current_buffer.len());
        Ok(read_bytes)
    }

    pub fn get_stream_position(&self) -> usize {
        match self.pos {
            Position::FrontBuffer(pos) => self.buffer_begins_at_pos + self.older_buffer.len() + pos,
            Position::BackBuffer(pos) => self.buffer_begins_at_pos + pos,
        }
    }

    /// Returns how many bytes the cursor can be moved backwards within the cache.
    pub fn max_rewind(&self) -> usize {
        match self.pos {
            Position::FrontBuffer(pos) => self.older_buffer.len() + pos,
            Position::BackBuffer(pos) => pos,
        }
    }

    fn seek_backwards(&mut self, shift: usize) -> Result<u64> {
        if shift > self.max_rewind() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                SeekError::RewindTooFar {
                    requested: self.get_stream_position().saturating_sub(shift) as u64,
                    earliest_possible: self.buffer_begins_at_pos as u64,
                },
            ));
        }

        let mut shift = shift;
        if let Position::FrontBuffer(pos) = self.pos {
            if shift > pos {
                self.pos = Position::BackBuffer(self.older_buffer.len() - 1);
                shift -= pos + 1;
            } else {
                self.pos = Position::FrontBuffer(pos - shift);
                return Ok(self.get_stream_position() as u64);
            }
        }

        if let Position::BackBuffer(pos) = self.pos {
            self.pos = Position::BackBuffer(pos - shift);
        }

        Ok(self.get_stream_position() as u64)
    }

    fn seek_forwards(&mut self, shift: usize) -> Result<u64> {
        let mut shift = shift;
        if let Position::BackBuffer(pos) = self.pos {
            let remaining_in_back_buffer = self.older_buffer.len() - pos;
            if shift >= remaining_in_back_buffer {
                self.pos = Position::FrontBuffer(0);
                shift -= remaining_in_back_buffer;
            } else {
                self.pos = Position::BackBuffer(pos + shift);
            }
        }

        if let Position::FrontBuffer(pos) = self.pos {
            let remaining_in_front_buffer = self.current_buffer.len() - pos;
            if shift > remaining_in_front_buffer {
                // We have to read additional data the user is not (yet) interested in
                shift -= remaining_in_front_buffer;
                self.pos = Position::FrontBuffer(self.current_buffer.len());
                let mut _discarded_data = vec![0; shift];
                self.read_inner(&mut _discarded_data)?;
            } else {
                self.pos = Position::FrontBuffer(pos + shift);
            }
        }

        Ok(self.get_stream_position() as u64)
    }
}

/// A SeekableReader can be read just normally:
///  ```
/// use std::io::Read;
/// use seekable_reader::SeekableReader;
///
/// let source = vec![1, 2, 3, 4, 5];
/// let reader = SeekableReader::new(source.as_slice(), 1);
/// let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
/// assert_eq!(&source, &bytes);
/// ```
impl<R: Read> Read for SeekableReader<R> {
    /// Read something from this source and write it into buffer, returning how many bytes were read.
    ///
    /// `read` will never read more than `buf.len()` from the underlying reader. But it may have read less
    /// than it returns, in case the user seeked backwards before, causing the cache to be used.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.pos {
            Position::FrontBuffer(pos) => {
                let cached = &self.current_buffer[pos..];
                let (from_cache, from_inner) = buf.split_at_mut(min(cached.len(), buf.len()));
                from_cache.copy_from_slice(&cached[..from_cache.len()]);
                self.pos = Position::FrontBuffer(pos + from_cache.len());
                if !from_inner.is_empty() {
                    Ok(from_cache.len() + self.read_inner(from_inner)?)
                } else {
                    Ok(from_cache.len())
                }
            }
            Position::BackBuffer(pos) => {
                let cached = &self.older_buffer[pos..];
                let cached = &cached[..min(cached.len(), buf.len())];
                let (from_cache, other) = buf.split_at_mut(min(cached.len(), buf.len()));
                from_cache.copy_from_slice(cached);
                if !other.is_empty() {
                    self.pos = Position::FrontBuffer(0);
                    Ok(from_cache.len() + self.read(other)?)
                } else {
                    self.pos = Position::BackBuffer(pos + cached.len());
                    Ok(from_cache.len())
                }
            }
        }
    }
}

impl<R: Read> Seek for SeekableReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let old_position = self.get_stream_position();
        match pos {
            SeekFrom::Start(pos) => {
                let pos = pos as usize;
                if let Some(diff) = pos.checked_sub(old_position) {
                    self.seek_forwards(diff)
                } else {
                    self.seek_backwards(old_position - pos)
                }
            }
            SeekFrom::End(_) => Err(Error::new(
                ErrorKind::Unsupported,
                SeekError::SeekFromEndUnsupported,
            )),
            SeekFrom::Current(shift) if shift > 0 => self.seek_forwards(shift as usize),
            SeekFrom::Current(shift) => self.seek_backwards((-shift) as usize),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{SeekError, SeekableReader};
    use std::io::{Read, Seek, SeekFrom};

    #[derive(Debug)]
    struct CountingReader {
        data: Vec<u8>,
        pos: usize,
        reads: usize,
    }

    impl CountingReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                pos: 0,
                reads: 0,
            }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn readthrough_1byte_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = SeekableReader::new(source.as_slice(), 1);
        let mut buffer = [0; 1];
        let mut dest = vec![];
        while reader.read(&mut buffer).unwrap() != 0 {
            dest.push(buffer[0]);
        }
        assert_eq!(dest, source);
    }

    #[test]
    fn readthrough_2byte_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = SeekableReader::new(source.as_slice(), 2);
        let mut buffer = [0; 1];
        let mut dest = vec![];
        while reader.read(&mut buffer).unwrap() != 0 {
            dest.push(buffer[0]);
        }
        assert_eq!(dest, source);
    }

    #[test]
    fn readall_5byte() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = SeekableReader::new(source.as_slice(), 5);
        let mut dest = [0; 5];
        reader.read(&mut dest).unwrap();
        assert_eq!(reader.older_buffer.len(), 5);
        assert_eq!(reader.current_buffer.len(), 0);
    }

    #[test]
    fn seek_small_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = SeekableReader::new(source.as_slice(), 1);
        let mut dest = vec![];
        let mut buffer = [0; 1];
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_forwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        assert_eq!(dest, [1, 1, 3]);
    }

    #[test]
    fn seek_2bytes_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = SeekableReader::new(source.as_slice(), 2);
        let mut dest = vec![];
        let mut buffer = [0; 1];
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(2).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_forwards(2).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        assert_eq!(dest, [1, 2, 1, 4, 4]);
    }

    #[test]
    fn bigger_test() {
        let source: Vec<u8> = (0..1536).map(|n| (n % 256) as u8).collect();
        let mut reader = SeekableReader::new(source.as_slice(), 1024);
        let mut buffer = [0; 1536];
        reader.read(&mut buffer).unwrap();
        assert_eq!(source.len(), buffer.len());
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::Current(-1024)).unwrap();
        reader.seek(SeekFrom::Current(-512)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
    }

    #[test]
    fn small_result_test() {
        let source: Vec<u8> = (0..1536).map(|n| (n % 256) as u8).collect();
        let mut reader = SeekableReader::new(source.as_slice(), 2048);
        let mut buf = [0; 27];
        assert_eq!(reader.read(&mut buf).unwrap(), 27);
    }

    #[test]
    fn seek_into_negative_pos() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 2);
        let err = reader.seek(SeekFrom::Current(-1)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(matches!(
            *err.into_inner().unwrap().downcast::<SeekError>().unwrap(),
            SeekError::RewindTooFar {
                requested: 0,
                earliest_possible: 0
            }
        ));
    }

    #[test]
    fn reread_whole_cache() {
        let source = [1, 2].as_slice();
        // keep_size=2 so both bytes remain in cache after reading
        let mut reader = SeekableReader::new(source, 2);
        reader.seek(SeekFrom::Current(2)).unwrap();
        reader.rewind().unwrap();

        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn seek_back_too_far() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 1);
        reader.seek(SeekFrom::Current(3)).unwrap();
        // Only 1 byte buffered (keep_size=1), seeking back 3 bytes must fail.
        // buffer_begins_at_pos=2, so earliest reachable position is 2, not 0.
        let err = reader.rewind().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(matches!(
            *err.into_inner().unwrap().downcast::<SeekError>().unwrap(),
            SeekError::RewindTooFar {
                requested: 0,
                earliest_possible: 2
            }
        ));
    }

    #[test]
    fn seek_to_end() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 2);
        reader.seek(SeekFrom::Current(3)).unwrap();
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn seek_from_end_unsupported() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 3);
        let err = reader.seek(SeekFrom::End(-1)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        let inner = err.into_inner().unwrap();
        assert!(inner.downcast::<SeekError>().is_ok());
    }

    #[test]
    fn seek_forwards_within_back_buffer() {
        // Covers the branch: seek_forwards from BackBuffer with shift < remaining,
        // i.e. the cursor stays inside older_buffer without crossing into current_buffer.
        let source = [1, 2, 3, 4, 5].as_slice();
        let mut reader = SeekableReader::new(source, 5);
        reader.seek(SeekFrom::Current(5)).unwrap(); // read all → older=[1..5], FrontBuffer(0)
        reader.seek(SeekFrom::Start(0)).unwrap(); // → BackBuffer(0)
        reader.seek(SeekFrom::Current(1)).unwrap(); // shift=1 < remaining=5 → BackBuffer(1)
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 2); // stream position 1 = value 2
    }

    #[test]
    fn read_after_flush_caches_tail() {
        // Covers the flush branch of read_inner: when a single read fills more than
        // 2*keep_size bytes, only the tail is kept. Verifies the cached byte is correct.
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 1); // cache_capacity = 2
        let mut buf = [0; 3];
        assert_eq!(reader.read(&mut buf).unwrap(), 3); // triggers flush: older=[3], pos=3
        assert_eq!(buf, [1, 2, 3]);
        // Only byte at position 2 (=3) remains in cache; one step back must succeed.
        reader.seek(SeekFrom::Current(-1)).unwrap();
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 3);
    }

    #[test]
    fn seek_start_forwards() {
        // Covers SeekFrom::Start where target > current position (forwards path).
        let source = [1, 2, 3, 4, 5].as_slice();
        let mut reader = SeekableReader::new(source, 5);
        let pos = reader.seek(SeekFrom::Start(3)).unwrap();
        assert_eq!(pos, 3);
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 4); // stream position 3 = value 4
    }

    #[test]
    fn seek_forwards_too_far() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 2);
        reader.seek(SeekFrom::Current(8)).unwrap();
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn seek_current_zero_is_noop() {
        let source = [1, 2, 3].as_slice();
        let mut reader = SeekableReader::new(source, 2);
        reader.seek(SeekFrom::Current(1)).unwrap();
        let before = reader.get_stream_position();
        let pos = reader.seek(SeekFrom::Current(0)).unwrap();
        assert_eq!(pos as usize, before);
        let mut buf = [0; 1];
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], 2);
    }

    #[test]
    fn seek_forwards_backbuffer_equal_remaining_boundary() {
        let source = [1, 2, 3, 4, 5].as_slice();
        let mut reader = SeekableReader::new(source, 5);
        reader.seek(SeekFrom::Current(5)).unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        let pos = reader.seek(SeekFrom::Current(5)).unwrap();
        assert_eq!(pos, 5);
        assert!(matches!(reader.pos, super::Position::FrontBuffer(0)));
    }

    #[test]
    fn seek_forwards_frontbuffer_equal_remaining_boundary() {
        let inner = CountingReader::new(vec![1, 2, 3, 4]);
        let mut reader = SeekableReader::new(inner, 4);

        let mut buf = [0; 3];
        assert_eq!(reader.read(&mut buf).unwrap(), 3);
        assert_eq!(buf, [1, 2, 3]);

        reader.seek(SeekFrom::Current(-2)).unwrap();
        let reads_before = reader.inner.reads;
        // FrontBuffer remaining is exactly 2 here, so this should not trigger read_inner.
        let pos = reader.seek(SeekFrom::Current(2)).unwrap();
        assert_eq!(pos, 3);
        assert_eq!(reader.inner.reads, reads_before);
    }

    #[test]
    fn read_inner_flush_equality_boundary() {
        // keep_size=1 -> cache_capacity=2, reading 2 bytes from empty current_buffer
        // hits the exact equality branch: read_bytes == cache_capacity - current.len().
        let source = [10, 11].as_slice();
        let mut reader = SeekableReader::new(source, 1);
        let mut buf = [0; 2];
        assert_eq!(reader.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [10, 11]);
        assert_eq!(reader.buffer_begins_at_pos, 1);
        assert_eq!(reader.older_buffer, vec![11]);
        assert_eq!(reader.current_buffer.len(), 0);
    }
}
