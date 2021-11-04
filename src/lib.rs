use std::mem;
use std::io::{Read, Result};

/// A reader adapter that allows to seek a little bit
pub struct PreservingReader<R: Read> {
    pub inner: R,
    current_buffer: Vec<u8>,
    older_buffer: Vec<u8>,
    pos: usize,
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
            pos: 0,
            read_bytes: 0,
        }
    }

    // Returns the number of bytes which can be read before the next buffer swap.
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
            self.current_buffer.extend_from_slice(&buf[..self.remaining_current_buffer_capacity()]);
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
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.pos < self.current_buffer.len() {
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

//impl<R: Read> Seek for PreservingReader<R> {
  //  fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        

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
