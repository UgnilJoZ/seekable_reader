use std::io::{Read, Result, Seek, SeekFrom};
use std::mem;

enum Position {
    FrontBuffer(usize),
    BackBuffer(usize),
}

/// A reader adapter that allows to seek a little bit
///
/// The PreservingReader will wrap around a Read instance and can be read normally.
/// The core feature is to provide `Seek`, even if the underlying Reader does not.
/// It achieves this by holding a cache of the read data, which can be read again.
///
///
/// ```
/// fn onebyte_buffer_readthrough() {
///     let source = vec![1, 2, 3, 4, 5];
///     let reader = PreservingReader::new(source.as_slice(), 1);
///     let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
///     assert_eq!(&source, &bytes);
/// }
/// ```
pub struct PreservingReader<R: Read> {
    pub inner: R,
    current_buffer: Vec<u8>,
    older_buffer: Vec<u8>,
    pos: Position,
    /// Bytes read from `inner`
    read_bytes: usize,
}

impl<R: Read> PreservingReader<R> {
    /// Create a new instance of a PreservingReader.
    ///
    /// It wraps around `inner` and allows seeking backwards by
    /// keeping at least `keep_size` bytes of already read data,
    /// if this amount of data is already read.
    ///
    /// At most, `2 * keep_size` bytes are kept.
    pub fn new(inner: R, keep_size: usize) -> PreservingReader<R> {
        PreservingReader {
            inner,
            current_buffer: Vec::with_capacity(keep_size),
            older_buffer: Vec::with_capacity(keep_size),
            pos: Position::FrontBuffer(0),
            read_bytes: 0,
        }
    }

    // Returns the number of bytes which can be read from inner before the next buffer swap.
    fn remaining_current_buffer_capacity(&self) -> usize {
        self.current_buffer.capacity() - self.current_buffer.len()
    }

    /// Returns the size of the buffered data.
    /// Attempts to seek further back will result an Error.
    pub fn buffered_size(&self) -> usize {
        self.current_buffer.len() + self.older_buffer.len()
    }

    /// Reads more data from `inner` into `buf` and puts them into the cache
    fn read_inner(&mut self, buf: &mut [u8]) -> Result<usize> {
        let read_bytes = self.inner.read(buf)?;
        if read_bytes >= self.remaining_current_buffer_capacity() {
            self.current_buffer
                .extend_from_slice(&buf[..self.remaining_current_buffer_capacity()]);
            self.older_buffer.clear();
            mem::swap(&mut self.current_buffer, &mut self.older_buffer);
            self.pos = 0;
        } else {
            self.current_buffer.extend_from_slice(&buf);
        }
        self.read_bytes += read_bytes;
        Ok(read_bytes)
    }
}

impl<R: Read> Read for PreservingReader<R> {
    /// Read something from this source and write it into buffer, returning how many bytes were read.
    ///
    /// `read` will never read more than `buf.len()` from the underlying reader. But it may have read less
    /// than it returns, in case the user seeked backwards before.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        // First, handle the older buffer / back buffer, if needed
        let front_pos = match self.pos {
            Position::FrontBuffer(pos) => pos,
            Position::BackBuffer(pos) => {
                let remaining_backbuffer = &self.older_buffer[pos..];
                if buf.len() < remaining_backbuffer.len() {
                    buf.copy_from_slice(remaining_backbuffer[..buf.len()]);
                    self.pos = Position::BackBuffer(pos + buf.len());
                } else {
                    let (backbuffer_cached, remainder) =
                        buf.split_at_mut(remaining_backbuffer.len());
                    backbuffer_cached.copy_from_slice(remaining_backbuffer);
                    self.pos = Position::FrontBuffer(0);
                    0
                }
            }
        };
        // Now, we can read the rest (which may involve the front buffer)
        if front_pos < self.current_buffer.len() {
            let buffer_remainder = &self.current_buffer[self.pos..];
            let (cached_answer, new_read) = buf.split_at_mut(buffer_remainder.len());
            cached_answer.copy_from_slice(&buffer_remainder);
            let newly_read = self.read_inner(new_read)?;
            self.pos = self.current_buffer.len();
            Ok(cached_answer.len() + newly_read)
        } else {
            self.read_inner(buf)
        }
    }
}

// impl<R: Read> Seek for PreservingReader<R> {
//     fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
//         ;
//     }
// }

#[cfg(test)]
mod tests {
    use crate::PreservingReader;
    use std::io::Read;

    #[test]
    fn onebyte_buffer_readthrough() {
        let source = vec![1, 2, 3, 4, 5];
        let reader = PreservingReader::new(source.as_slice(), 1);
        let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
        assert_eq!(&source, &bytes);
    }
}
