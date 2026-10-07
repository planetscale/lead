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
use crate::TokenizerPipelineSpecError;
use rust_stemmers::Algorithm;
use std::{fmt, str::FromStr};

/// A Snowball language selected by its ISO 639-1 code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stemmer {
    Arabic,
    Danish,
    Dutch,
    English,
    Finnish,
    French,
    German,
    Greek,
    Hungarian,
    Italian,
    Norwegian,
    Portuguese,
    Romanian,
    Russian,
    Spanish,
    Swedish,
    Tamil,
    Turkish,
}

impl Stemmer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arabic => "ar",
            Self::Danish => "da",
            Self::Dutch => "nl",
            Self::English => "en",
            Self::Finnish => "fi",
            Self::French => "fr",
            Self::German => "de",
            Self::Greek => "el",
            Self::Hungarian => "hu",
            Self::Italian => "it",
            Self::Norwegian => "no",
            Self::Portuguese => "pt",
            Self::Romanian => "ro",
            Self::Russian => "ru",
            Self::Spanish => "es",
            Self::Swedish => "sv",
            Self::Tamil => "ta",
            Self::Turkish => "tr",
        }
    }

    pub(crate) fn compile(self) -> rust_stemmers::Stemmer {
        rust_stemmers::Stemmer::create(match self {
            Self::Arabic => Algorithm::Arabic,
            Self::Danish => Algorithm::Danish,
            Self::Dutch => Algorithm::Dutch,
            Self::English => Algorithm::English,
            Self::Finnish => Algorithm::Finnish,
            Self::French => Algorithm::French,
            Self::German => Algorithm::German,
            Self::Greek => Algorithm::Greek,
            Self::Hungarian => Algorithm::Hungarian,
            Self::Italian => Algorithm::Italian,
            Self::Norwegian => Algorithm::Norwegian,
            Self::Portuguese => Algorithm::Portuguese,
            Self::Romanian => Algorithm::Romanian,
            Self::Russian => Algorithm::Russian,
            Self::Spanish => Algorithm::Spanish,
            Self::Swedish => Algorithm::Swedish,
            Self::Tamil => Algorithm::Tamil,
            Self::Turkish => Algorithm::Turkish,
        })
    }
}

impl FromStr for Stemmer {
    type Err = TokenizerPipelineSpecError;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Ok(match code {
            "ar" => Self::Arabic,
            "da" => Self::Danish,
            "nl" => Self::Dutch,
            "en" => Self::English,
            "fi" => Self::Finnish,
            "fr" => Self::French,
            "de" => Self::German,
            "el" => Self::Greek,
            "hu" => Self::Hungarian,
            "it" => Self::Italian,
            "no" => Self::Norwegian,
            "pt" => Self::Portuguese,
            "ro" => Self::Romanian,
            "ru" => Self::Russian,
            "es" => Self::Spanish,
            "sv" => Self::Swedish,
            "ta" => Self::Tamil,
            "tr" => Self::Turkish,
            _ => return Err(TokenizerPipelineSpecError::UnknownStemmer(code.to_owned())),
        })
    }
}

impl fmt::Display for Stemmer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
