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
use crate::compiled::CompiledTokenizerPipeline;

/// Smallest byte ceiling that can contain every UTF-8 scalar value.
pub const MIN_TOKEN_BYTES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenizerPipelineSpec {
    pub tokenizer: TokenizerSpec,
    pub case_folding: Folding,
    pub accent_folding: Folding,
    pub long_tokens: LongTokenSpec,
    pub graphemes: GraphemeMode,
    pub position_gaps: PositionGapMode,
}

impl Default for TokenizerPipelineSpec {
    fn default() -> Self {
        Self::tin_default()
    }
}

impl TokenizerPipelineSpec {
    pub const fn tin_default() -> Self {
        Self {
            tokenizer: TokenizerSpec::Unicode,
            case_folding: Folding::Fold,
            accent_folding: Folding::Fold,
            long_tokens: LongTokenSpec {
                mode: LongTokenMode::Split,
                max_bytes: 256,
            },
            graphemes: GraphemeMode::Emoji,
            position_gaps: PositionGapMode::Preserve,
        }
    }

    /// The fixed pipeline used by tin before per-index tokenization options
    /// existed. Regression fixtures use this tuple when they need to keep
    /// testing their historical corpus and scoring behavior.
    pub const fn legacy_tin_default() -> Self {
        Self {
            tokenizer: TokenizerSpec::Unicode,
            case_folding: Folding::Fold,
            accent_folding: Folding::Preserve,
            long_tokens: LongTokenSpec {
                mode: LongTokenMode::Truncate,
                max_bytes: 256,
            },
            graphemes: GraphemeMode::Discard,
            position_gaps: PositionGapMode::Collapse,
        }
    }

    pub fn validate(self) -> Result<(), TokenizerPipelineSpecError> {
        if self.long_tokens.max_bytes < MIN_TOKEN_BYTES {
            return Err(TokenizerPipelineSpecError::MaxTokenBytesTooSmall);
        }
        Ok(())
    }

    pub fn compile(self) -> Result<CompiledTokenizerPipeline, TokenizerPipelineSpecError> {
        self.validate()?;
        Ok(CompiledTokenizerPipeline::from_validated_spec(self))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerSpec {
    Unicode,
    Whitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folding {
    Preserve,
    Fold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongTokenMode {
    Truncate,
    Discard,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongTokenSpec {
    pub mode: LongTokenMode,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphemeMode {
    Discard,
    Emoji,
    Retain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionGapMode {
    Collapse,
    Preserve,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenizerPipelineSpecError {
    #[error("max_token_bytes must be at least {MIN_TOKEN_BYTES}")]
    MaxTokenBytesTooSmall,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_sql_default_tuple() {
        assert_eq!(
            TokenizerPipelineSpec::tin_default(),
            TokenizerPipelineSpec {
                tokenizer: TokenizerSpec::Unicode,
                case_folding: Folding::Fold,
                accent_folding: Folding::Fold,
                long_tokens: LongTokenSpec {
                    mode: LongTokenMode::Split,
                    max_bytes: 256,
                },
                graphemes: GraphemeMode::Emoji,
                position_gaps: PositionGapMode::Preserve,
            }
        );
    }

    #[test]
    fn validate_rejects_a_ceiling_smaller_than_one_utf8_scalar() {
        let mut spec = TokenizerPipelineSpec::tin_default();
        spec.long_tokens.max_bytes = MIN_TOKEN_BYTES - 1;
        assert!(matches!(
            spec.validate(),
            Err(TokenizerPipelineSpecError::MaxTokenBytesTooSmall)
        ));
    }

    #[test]
    fn legacy_default_pins_pre_reloptions_behavior() {
        assert_eq!(
            TokenizerPipelineSpec::legacy_tin_default(),
            TokenizerPipelineSpec {
                tokenizer: TokenizerSpec::Unicode,
                case_folding: Folding::Fold,
                accent_folding: Folding::Preserve,
                long_tokens: LongTokenSpec {
                    mode: LongTokenMode::Truncate,
                    max_bytes: 256,
                },
                graphemes: GraphemeMode::Discard,
                position_gaps: PositionGapMode::Collapse,
            }
        );
    }
}
