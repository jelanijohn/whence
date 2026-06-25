//! The core: segmentation (decide current focus + real switches) and the local
//! timeline store. `segment` is pure and fixture-tested; `timeline` is the JSONL
//! persistence layer. The orchestration that wires adapters → segmenter → outputs
//! lives in `crate::core`.

pub mod segment;
pub mod timeline;
