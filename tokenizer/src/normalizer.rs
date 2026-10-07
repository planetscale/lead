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
use crate::folder::CompiledFolder;
use crate::{Folding, Stemmer, Token};
use std::borrow::Cow;
use unicode_normalization::{UnicodeNormalization, is_nfc};

#[derive(Clone, Copy)]
pub(crate) enum NormalizerSpec {
    Fold(CompiledFolder),
    Stem(Stemmer, Folding),
}

impl NormalizerSpec {
    pub(crate) fn new(case: Folding, accent: Folding, stemmer: Option<Stemmer>) -> Self {
        match stemmer {
            Some(stemmer) => Self::Stem(stemmer, accent),
            None => Self::Fold(CompiledFolder::new(case, accent)),
        }
    }

    pub(crate) fn compile(self) -> CompiledNormalizer {
        match self {
            Self::Fold(folder) => CompiledNormalizer::Fold(folder),
            Self::Stem(stemmer, accent) => CompiledNormalizer::Stem {
                stemmer: stemmer.compile(),
                accent: CompiledFolder::new(Folding::Preserve, accent),
            },
        }
    }
}

pub(crate) enum CompiledNormalizer {
    Fold(CompiledFolder),
    Stem {
        stemmer: rust_stemmers::Stemmer,
        accent: CompiledFolder,
    },
}

impl CompiledNormalizer {
    pub(crate) fn apply(&self, token: &mut Token<'_>) {
        match self {
            Self::Fold(folder) => folder.apply(token),
            Self::Stem { stemmer, accent } => {
                CompiledFolder::Case.apply(token);
                // Snowball suffix rules must see the same accents for canonically
                // equivalent words; keep accent removal after stemming.
                if !is_nfc(&token.text) {
                    token.text = Cow::Owned(token.text.nfc().collect());
                }
                match &mut token.text {
                    Cow::Borrowed(text) => token.text = stemmer.stem(text),
                    Cow::Owned(text) => {
                        // A borrowed stem is unchanged. Keep the existing
                        // lowercase allocation instead of copying it again.
                        if let Cow::Owned(stemmed) = stemmer.stem(text) {
                            *text = stemmed;
                        }
                    }
                }
                accent.apply(token);
            }
        }
    }
}
