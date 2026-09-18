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
use std::sync::LazyLock;

use crate::{CompiledTokenizerPipeline, TokenizerPipelineSpec};

/// Stable default tokenizer pipeline spec used by tin for indexing and query evaluation.
pub fn default_pipeline_spec() -> &'static TokenizerPipelineSpec {
    static SPEC: LazyLock<TokenizerPipelineSpec> =
        LazyLock::new(TokenizerPipelineSpec::tin_default);
    &SPEC
}

/// Lazily compiled default tokenization pipeline used by tin for indexing and query evaluation.
pub fn default_pipeline() -> &'static CompiledTokenizerPipeline {
    static PIPELINE: LazyLock<CompiledTokenizerPipeline> = LazyLock::new(|| {
        (*default_pipeline_spec())
            .compile()
            .expect("tin default tokenizer pipeline spec should always compile")
    });
    &PIPELINE
}
