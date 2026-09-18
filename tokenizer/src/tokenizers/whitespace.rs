// Copyright (C) 2026 PlanetScale
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.
//
// The full license text is available in LICENSE.
use crate::{Token, Tokenizer};

pub struct Whitespace;

pub struct WhitespaceIter<'a> {
    inner: std::str::SplitWhitespace<'a>,
    pos: u32,
}

impl<'a> WhitespaceIter<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            inner: text.split_whitespace(),
            pos: 0,
        }
    }
}

impl Tokenizer for Whitespace {
    type Iter<'tokenizer, 'text>
        = WhitespaceIter<'text>
    where
        Self: 'tokenizer;

    fn tokenize<'tokenizer, 'text>(
        &'tokenizer self,
        text: &'text str,
    ) -> Self::Iter<'tokenizer, 'text> {
        WhitespaceIter::new(text)
    }
}

impl<'a> Iterator for WhitespaceIter<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let word = self.inner.next()?;
        let pos = self.pos;
        self.pos += 1;
        Some(Token::new(word, pos))
    }
}

#[cfg(test)]
mod tests {
    use crate::tokenizers::Whitespace;
    use crate::{Classification, Token, Tokenizer};

    #[test]
    fn test_whitespace() {
        let whitespace = Whitespace;
        let mut tokenizer = whitespace.tokenize("Hello, world!");
        assert_eq!(
            tokenizer.next().unwrap(),
            Token::with_classification("Hello,", 0, Classification::Unknown)
        );
        assert_eq!(
            tokenizer.next().unwrap(),
            Token::with_classification("world!", 1, Classification::Unknown)
        );
        assert_eq!(tokenizer.next(), None);
    }
}
