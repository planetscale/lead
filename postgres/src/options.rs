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
use pgrx::{pg_guard, pg_sys};
use std::ffi::CStr;
use std::sync::atomic::{AtomicU32, Ordering};
use tokenizer::{
    Folding, GraphemeMode, LongTokenMode, LongTokenSpec, PositionGapMode, TokenizerPipelineSpec,
    TokenizerSpec,
};

use crate::bm25::Bm25Params;
use crate::udfs::MAX_TOKEN_BYTES;

const TOKENIZER_UNICODE: i32 = 0;
const TOKENIZER_WHITESPACE: i32 = 1;
const FOLDING_PRESERVE: i32 = 0;
const FOLDING_FOLD: i32 = 1;
const LONG_TRUNCATE: i32 = 0;
const LONG_DISCARD: i32 = 1;
const LONG_SPLIT: i32 = 2;
const GRAPHEME_DISCARD: i32 = 0;
const GRAPHEME_EMOJI: i32 = 1;
const GRAPHEME_RETAIN: i32 = 2;
const GAPS_COLLAPSE: i32 = 0;
const GAPS_PRESERVE: i32 = 1;

static OPTION_KIND: AtomicU32 = AtomicU32::new(0);

macro_rules! enum_members {
    ($name:ident, $(($text:literal, $value:expr)),+ $(,)?) => {
        static mut $name: [pg_sys::relopt_enum_elt_def; enum_members!(@count $(($text, $value)),+) + 1] = [
            $(pg_sys::relopt_enum_elt_def {
                string_val: concat!($text, "\0").as_ptr().cast(),
                symbol_val: $value,
            },)+
            pg_sys::relopt_enum_elt_def {
                string_val: std::ptr::null(),
                symbol_val: 0,
            },
        ];
    };
    (@count $(($text:literal, $value:expr)),+) => {
        <[()]>::len(&[$(enum_members!(@one $text $value)),+])
    };
    (@one $text:literal $value:expr) => { () };
}

enum_members!(
    TOKENIZERS,
    ("unicode", TOKENIZER_UNICODE),
    ("whitespace", TOKENIZER_WHITESPACE)
);
enum_members!(
    FOLDINGS,
    ("preserve", FOLDING_PRESERVE),
    ("fold", FOLDING_FOLD)
);
enum_members!(
    LONG_MODES,
    ("truncate", LONG_TRUNCATE),
    ("discard", LONG_DISCARD),
    ("split", LONG_SPLIT)
);
enum_members!(
    GRAPHEME_MODES,
    ("discard", GRAPHEME_DISCARD),
    ("emoji", GRAPHEME_EMOJI),
    ("retain", GRAPHEME_RETAIN)
);
enum_members!(
    GAP_MODES,
    ("collapse", GAPS_COLLAPSE),
    ("preserve", GAPS_PRESERVE)
);

#[repr(C)]
struct IndexOptions {
    varlena_header: i32,
    initial_segment_count: i32,
    tokenizer: i32,
    case_folding: i32,
    accent_folding: i32,
    long_tokens: i32,
    max_token_bytes: i32,
    graphemes: i32,
    position_gaps: i32,
    k1: f64,
    b: f64,
    score_stop_words: i32,
}

pub fn init() {
    if OPTION_KIND.load(Ordering::Relaxed) != 0 {
        return;
    }
    let lock = pg_sys::ShareUpdateExclusiveLock as pg_sys::LOCKMODE;
    unsafe {
        let kind = pg_sys::add_reloption_kind();
        pg_sys::add_int_reloption(
            kind,
            c"initial_segment_count".as_ptr(),
            c"Ignored Lead segment-count compatibility option".as_ptr(),
            1,
            1,
            1024,
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"tokenizer".as_ptr(),
            c"Token boundary policy".as_ptr(),
            (&raw mut TOKENIZERS).cast(),
            TOKENIZER_UNICODE,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"case_folding".as_ptr(),
            c"Case folding policy".as_ptr(),
            (&raw mut FOLDINGS).cast(),
            FOLDING_FOLD,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"accent_folding".as_ptr(),
            c"Accent folding policy".as_ptr(),
            (&raw mut FOLDINGS).cast(),
            FOLDING_FOLD,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"long_tokens".as_ptr(),
            c"Long-token policy".as_ptr(),
            (&raw mut LONG_MODES).cast(),
            LONG_SPLIT,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_int_reloption(
            kind,
            c"max_token_bytes".as_ptr(),
            c"Maximum analyzed token length".as_ptr(),
            256,
            tokenizer::MIN_TOKEN_BYTES as i32,
            MAX_TOKEN_BYTES as i32,
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"graphemes".as_ptr(),
            c"Standalone grapheme policy".as_ptr(),
            (&raw mut GRAPHEME_MODES).cast(),
            GRAPHEME_EMOJI,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"position_gaps".as_ptr(),
            c"Position policy for removed tokens".as_ptr(),
            (&raw mut GAP_MODES).cast(),
            GAPS_PRESERVE,
            std::ptr::null(),
            lock,
        );
        pg_sys::add_real_reloption(
            kind,
            c"k1".as_ptr(),
            c"BM25 term-frequency saturation".as_ptr(),
            f64::from(Bm25Params::DEFAULT_K1),
            0.0,
            f64::from(Bm25Params::K1_MAX),
            lock,
        );
        pg_sys::add_real_reloption(
            kind,
            c"b".as_ptr(),
            c"BM25 document-length normalization".as_ptr(),
            f64::from(Bm25Params::DEFAULT_B),
            0.0,
            1.0,
            lock,
        );
        pg_sys::add_string_reloption(
            kind,
            c"score_stop_words".as_ptr(),
            c"Comma-separated analyzed terms omitted by tin.score".as_ptr(),
            std::ptr::null(),
            None,
            lock,
        );
        OPTION_KIND.store(kind, Ordering::Relaxed);
    }
}

fn parse_entry(
    name: *const std::ffi::c_char,
    kind: pg_sys::relopt_type::Type,
    offset: usize,
) -> pg_sys::relopt_parse_elt {
    pg_sys::relopt_parse_elt {
        optname: name,
        opttype: kind,
        offset: offset as i32,
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    }
}

#[pg_guard]
pub unsafe extern "C-unwind" fn amoptions(
    reloptions: pg_sys::Datum,
    validate: bool,
) -> *mut pg_sys::bytea {
    let entries = [
        parse_entry(
            c"initial_segment_count".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            std::mem::offset_of!(IndexOptions, initial_segment_count),
        ),
        parse_entry(
            c"tokenizer".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, tokenizer),
        ),
        parse_entry(
            c"case_folding".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, case_folding),
        ),
        parse_entry(
            c"accent_folding".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, accent_folding),
        ),
        parse_entry(
            c"long_tokens".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, long_tokens),
        ),
        parse_entry(
            c"max_token_bytes".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            std::mem::offset_of!(IndexOptions, max_token_bytes),
        ),
        parse_entry(
            c"graphemes".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, graphemes),
        ),
        parse_entry(
            c"position_gaps".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            std::mem::offset_of!(IndexOptions, position_gaps),
        ),
        parse_entry(
            c"k1".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_REAL,
            std::mem::offset_of!(IndexOptions, k1),
        ),
        parse_entry(
            c"b".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_REAL,
            std::mem::offset_of!(IndexOptions, b),
        ),
        parse_entry(
            c"score_stop_words".as_ptr(),
            pg_sys::relopt_type::RELOPT_TYPE_STRING,
            std::mem::offset_of!(IndexOptions, score_stop_words),
        ),
    ];
    unsafe {
        pg_sys::build_reloptions(
            reloptions,
            validate,
            OPTION_KIND.load(Ordering::Relaxed),
            std::mem::size_of::<IndexOptions>(),
            entries.as_ptr(),
            entries.len() as i32,
        )
        .cast()
    }
}

unsafe fn parsed(index: pg_sys::Relation) -> Option<&'static IndexOptions> {
    if index.is_null() {
        return None;
    }
    unsafe { (*index).rd_options.cast::<IndexOptions>().as_ref() }
}

pub unsafe fn tokenizer_spec(index: pg_sys::Relation) -> TokenizerPipelineSpec {
    let Some(options) = (unsafe { parsed(index) }) else {
        return TokenizerPipelineSpec::tin_default();
    };
    TokenizerPipelineSpec {
        tokenizer: match options.tokenizer {
            TOKENIZER_WHITESPACE => TokenizerSpec::Whitespace,
            _ => TokenizerSpec::Unicode,
        },
        case_folding: decode_folding(options.case_folding),
        accent_folding: decode_folding(options.accent_folding),
        long_tokens: LongTokenSpec {
            mode: match options.long_tokens {
                LONG_TRUNCATE => LongTokenMode::Truncate,
                LONG_DISCARD => LongTokenMode::Discard,
                _ => LongTokenMode::Split,
            },
            max_bytes: options.max_token_bytes as usize,
        },
        graphemes: match options.graphemes {
            GRAPHEME_DISCARD => GraphemeMode::Discard,
            GRAPHEME_RETAIN => GraphemeMode::Retain,
            _ => GraphemeMode::Emoji,
        },
        position_gaps: match options.position_gaps {
            GAPS_COLLAPSE => PositionGapMode::Collapse,
            _ => PositionGapMode::Preserve,
        },
    }
}

fn decode_folding(value: i32) -> Folding {
    if value == FOLDING_PRESERVE {
        Folding::Preserve
    } else {
        Folding::Fold
    }
}

pub unsafe fn tokenizer(index: pg_sys::Relation) -> tokenizer::CompiledTokenizerPipeline {
    unsafe { tokenizer_spec(index) }
        .compile()
        .expect("catalog-validated tokenizer options")
}

pub unsafe fn bm25(index: pg_sys::Relation) -> Bm25Params {
    unsafe { parsed(index) }
        .map(|options| Bm25Params {
            k1: options.k1 as f32,
            b: options.b as f32,
        })
        .unwrap_or_else(Bm25Params::default_bm25)
}

pub unsafe fn score_stop_words(index: pg_sys::Relation) -> Option<String> {
    let options = unsafe { parsed(index) }?;
    let offset = usize::try_from(options.score_stop_words).ok()?;
    if offset == 0 {
        return None;
    }
    let ptr = std::ptr::from_ref(options).cast::<u8>();
    unsafe { CStr::from_ptr(ptr.add(offset).cast()) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_standalone_pipeline() {
        assert_eq!(
            TokenizerPipelineSpec::tin_default().long_tokens.max_bytes,
            256
        );
        assert_eq!(Bm25Params::default(), Bm25Params { k1: 1.2, b: 0.75 });
    }
}
