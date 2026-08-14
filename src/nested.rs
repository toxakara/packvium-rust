use crate::{PackResult, PackingRequest, PackingResult, pack_request};

#[derive(Clone, Debug)]
pub struct NestedLevel {
    pub name: String,
    pub request: PackingRequest,
}

#[derive(Clone, Debug)]
pub struct NestedPackingRequest {
    pub levels: Vec<NestedLevel>,
    pub beam_width: usize,
}

#[derive(Clone, Debug)]
pub struct NestedPackingResult {
    pub levels: Vec<(String, PackingResult)>,
}

/// Executes explicitly supplied levels in order. Cross-level transformation
/// and Pareto beam optimization are intentionally left to the caller in 0.1.0.
pub fn pack_nested(request: &NestedPackingRequest) -> PackResult<NestedPackingResult> {
    let mut levels = Vec::new();
    for level in &request.levels {
        levels.push((level.name.clone(), pack_request(&level.request)?));
    }
    Ok(NestedPackingResult { levels })
}
