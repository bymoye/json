use super::*;

/// JSON input backed by immutable byte chunks. Strings within one chunk borrow
/// that chunk; only strings spanning chunks or containing escapes use scratch.
pub struct ChunksRead<'a, I> {
    original: core::iter::Fuse<I>,
    remaining: core::iter::Fuse<I>,
    current: SliceRead<'a>,
    offset: usize,
    #[cfg(feature = "raw_value")]
    raw_start: usize,
}

impl<'a, I: Iterator<Item = &'a [u8]> + Clone> ChunksRead<'a, I> {
    /// Creates an input source without copying or collecting the chunks.
    pub fn new(chunks: I) -> Self {
        let chunks = chunks.fuse();
        Self {
            original: chunks.clone(),
            remaining: chunks,
            current: SliceRead::new(&[]),
            offset: 0,
            #[cfg(feature = "raw_value")]
            raw_start: 0,
        }
    }

    fn advance_chunk(&mut self) -> bool {
        while self.current.index == self.current.slice.len() {
            let Some(chunk) = self.remaining.next() else {
                return false;
            };
            self.offset += self.current.slice.len();
            self.current = SliceRead::new(chunk);
        }
        true
    }

    fn position_at(&self, mut index: usize) -> Position {
        let mut position = Position { line: 1, column: 0 };
        for chunk in self.original.clone() {
            for &byte in &chunk[..cmp::min(index, chunk.len())] {
                if byte == b'\n' {
                    position.line += 1;
                    position.column = 0;
                } else {
                    position.column += 1;
                }
            }
            if index <= chunk.len() {
                break;
            }
            index -= chunk.len();
        }
        position
    }

    fn parse_bytes<'s, T: ?Sized + 's, F>(
        &'s mut self,
        scratch: &'s mut Vec<u8>,
        validate: bool,
        result: F,
    ) -> Result<Reference<'a, 's, T>>
    where
        F: for<'f> FnOnce(&'s Self, &'f [u8]) -> Result<&'f T>,
    {
        let mut start = self.current.index;
        loop {
            self.current.skip_to_escape(validate);
            if self.current.index == self.current.slice.len() {
                scratch.extend_from_slice(&self.current.slice[start..]);
                if !self.advance_chunk() {
                    return error(self, ErrorCode::EofWhileParsingString);
                }
                start = 0;
                continue;
            }
            match self.current.slice[self.current.index] {
                b'"' => {
                    let bytes = &self.current.slice[start..self.current.index];
                    self.current.index += 1;
                    if scratch.is_empty() {
                        return result(self, bytes).map(Reference::Borrowed);
                    }
                    scratch.extend_from_slice(bytes);
                    return result(self, scratch).map(Reference::Copied);
                }
                b'\\' => {
                    scratch.extend_from_slice(&self.current.slice[start..self.current.index]);
                    self.current.index += 1;
                    tri!(parse_escape(self, validate, scratch));
                    start = self.current.index;
                }
                _ => {
                    self.current.index += 1;
                    return error(self, ErrorCode::ControlCharacterWhileParsingString);
                }
            }
        }
    }
}

impl<'a, I: Iterator<Item = &'a [u8]> + Clone> private::Sealed for ChunksRead<'a, I> {}
impl<'a, I: Iterator<Item = &'a [u8]> + Clone> Read<'a> for ChunksRead<'a, I> {
    fn next(&mut self) -> Result<Option<u8>> {
        if self.advance_chunk() {
            self.current.next()
        } else {
            Ok(None)
        }
    }
    fn peek(&mut self) -> Result<Option<u8>> {
        if self.advance_chunk() {
            self.current.peek()
        } else {
            Ok(None)
        }
    }
    fn discard(&mut self) {
        self.current.discard();
    }
    fn position(&self) -> Position {
        self.position_at(self.byte_offset())
    }
    fn peek_position(&self) -> Position {
        self.position_at(self.byte_offset() + 1)
    }
    fn byte_offset(&self) -> usize {
        self.offset + self.current.index
    }
    fn parse_str<'s>(&'s mut self, scratch: &'s mut Vec<u8>) -> Result<Reference<'a, 's, str>> {
        self.parse_bytes(scratch, true, as_str)
    }
    fn parse_str_raw<'s>(
        &'s mut self,
        scratch: &'s mut Vec<u8>,
    ) -> Result<Reference<'a, 's, [u8]>> {
        self.parse_bytes(scratch, false, |_, bytes| Ok(bytes))
    }
    fn ignore_str(&mut self) -> Result<()> {
        loop {
            self.current.skip_to_escape(true);
            if self.current.index == self.current.slice.len() {
                if !self.advance_chunk() {
                    return error(self, ErrorCode::EofWhileParsingString);
                }
                continue;
            }
            match self.current.slice[self.current.index] {
                b'"' => {
                    self.current.index += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.current.index += 1;
                    tri!(ignore_escape(self));
                }
                _ => return error(self, ErrorCode::ControlCharacterWhileParsingString),
            }
        }
    }
    fn decode_hex_escape(&mut self) -> Result<u16> {
        let mut digits = [0; 4];
        for digit in &mut digits {
            *digit = match tri!(self.next()) {
                Some(byte) => byte,
                None => return error(self, ErrorCode::EofWhileParsingString),
            };
        }
        match decode_four_hex_digits(digits[0], digits[1], digits[2], digits[3]) {
            Some(value) => Ok(value),
            None => error(self, ErrorCode::InvalidEscape),
        }
    }
    #[cfg(feature = "raw_value")]
    fn begin_raw_buffering(&mut self) {
        self.raw_start = self.byte_offset();
    }
    #[cfg(feature = "raw_value")]
    fn end_raw_buffering<V: Visitor<'a>>(&mut self, visitor: V) -> Result<V::Value> {
        let mut start = self.raw_start;
        let mut length = self.byte_offset() - start;
        let mut bytes = Vec::new();
        for chunk in self.original.clone() {
            if start >= chunk.len() {
                start -= chunk.len();
                continue;
            }
            let part = &chunk[start..start + cmp::min(length, chunk.len() - start)];
            if bytes.is_empty() && part.len() == length {
                let raw = str::from_utf8(part).map_err(|_| {
                    Error::syntax(
                        ErrorCode::InvalidUnicodeCodePoint,
                        self.position().line,
                        self.position().column,
                    )
                })?;
                return visitor.visit_map(BorrowedRawDeserializer {
                    raw_value: Some(raw),
                });
            }
            if bytes.is_empty() {
                bytes.reserve(length);
            }
            bytes.extend_from_slice(part);
            length -= part.len();
            start = 0;
            if length == 0 {
                break;
            }
        }
        let raw = String::from_utf8(bytes).map_err(|_| {
            Error::syntax(
                ErrorCode::InvalidUnicodeCodePoint,
                self.position().line,
                self.position().column,
            )
        })?;
        visitor.visit_map(OwnedRawDeserializer {
            raw_value: Some(raw),
        })
    }
    const should_early_return_if_failed: bool = true;
    fn set_failed(&mut self, failed: &mut bool) {
        *failed = true;
    }
}
impl<'a, I: Iterator<Item = &'a [u8]> + Clone> Fused for ChunksRead<'a, I> {}
